//! `vike-cli mcp` — a LOCAL stdio MCP server exposing the vike agent surface as tools, so Claude (or
//! any MCP client) can run the full loop: CREATE + BACKTEST (author a Rhai strategy, discover its
//! knobs, see the host-bound indicator set, run a backtest) AND — against a running vike-tradehub
//! node — read live state and place orders through a mandatory PREVIEW gate. Part of the "vike-cli
//! as the one agent surface" program
//! (`docs/superpowers/specs/2026-07-26-vike-cli-agent-surface-program.md`).
//!
//! # Transport (hand-rolled, no SDK)
//!
//! MCP's stdio transport is JSON-RPC 2.0 framed as NEWLINE-delimited JSON (one message per line, no
//! embedded newlines). We hand-roll it with `serde_json` — the SAME "no async, no new transport
//! stack, stdio-JSON" convention the dukascopy sidecar, vike-datahub, and vike-tradehub use (a full
//! MCP SDK would drag in tokio + a second async stack the workspace rejects). Requests carry an `id`
//! and get exactly one response; notifications (no `id`) get none.
//!
//! # Tools
//!
//! READ-ONLY / safe (`readOnlyHint`): `validate_strategy`, `discover_params`, `list_templates`,
//! `list_indicators` (all OFFLINE — pure vike-script, no server), `run_backtest`, `run_sweep`,
//! `run_walk_forward` (remote datahub runs — a backtest MUTATES nothing), `list_strategies`,
//! `list_series` (remote datahub metadata), `node_snapshot`. This is the ABSORBED `vike-mcp` tool
//! surface (Phase A of retiring that crate): the run/list tools go over the datahub RPC verbs
//! instead of a local DataFusion store, so this CLI stays DataFusion-free; `validate_strategy` /
//! `list_templates` are offline ports (note the argument is named `script` here — vike-mcp said
//! `code` — matching this file's existing `discover_params`).
//!
//! LIVE ORDER-WRITE (`destructiveHint`), gated: `submit_order`, `cancel_order`, `modify`,
//! `flatten`, `market_exit`, `set_trading_state`, `mass_cancel` — the FULL `trade`-REPL write-verb
//! roster, resolved through the SAME [`crate::cmd::verbs`] construction site + guardrail the REPL
//! uses (one vocabulary, two surfaces). **Mandatory preview, and it is now actually mandatory:** a
//! write executes ONLY against a `preview_token` this server issued, unexpired, and BOUND to the
//! same command. Any other shape — no token, unknown token, expired token, a token from a different
//! command, or `confirm: true` on its own — returns a preview and sends nothing.
//!
//! ⚠ This paragraph used to claim that gate while the code did not implement it. `confirm: true`
//! was the whole test, on THAT call, so a first-and-only call executed; nothing correlated a
//! confirm with the preview it claimed to confirm (previewing `qty: 0.5` and confirming `qty: 50`
//! was accepted); and the client-side guardrail it showed cannot price a MARKET order — there is no
//! `price` to size against — which is the DEFAULT order type. So the advertised protection was
//! absent on exactly the path most likely to be taken. The preview now also asks the NODE for its
//! own dry-run ([`Server::node_preview`]) and, when the node cannot be reached, SAYS the verdict is
//! an unverified client-side estimate rather than presenting an absence as approval. The server-side `ControlLimits` (notional + rate) and
//! the core `RiskGate` on the vike-tradehub node are the ENFORCING backstop regardless — the client
//! preview is UX, the node gate is truth. Writes go to a running node named by `--node <addr>` with
//! `VIKE_TRADEHUB_CONTROL_KEY` (reads need `VIKE_TRADEHUB_OBSERVE_KEY`) — taken from the process
//! environment, else from the credential store the daemon itself reads
//! (`<project>/settings/secrets.env`, see [`crate::cmd::nodekeys`]); absent from BOTH, the trade
//! tools return a clean error naming both places and the create+backtest tools still work.
//!
//! **Every `submit_order` carries a coid.** `client_order_id` stays an OPTIONAL argument, but the
//! node REFUSES a remote submit with an empty one, so when the agent omits it this server MINTS one
//! ([`verbs::fill_client_order_id`], the same generator and wire form the live core uses) before the
//! preview renders. The preview therefore shows a real id, and the confirming call SENDS THAT id:
//! [`Server::call_tool`] executes the STORED preview (`previewed.cmd`), not the command it rebuilt
//! from the confirming arguments, so the agent never needs to pass the id back — [`same_intent`]
//! ignores the minted id precisely so a confirm without it still matches. `client_order_id` in the
//! accepted response is the authoritative one, and it is the one the preview showed. (This paragraph
//! said the opposite — "mints its OWN unless the agent pins the previewed id back" — and three
//! published pages copied that sentence before the docs gate caught it.)
//!
//! **Each write tool reports ITS OWN command's outcome** — [`Server::execute`] awaits the
//! `CommandTicket` the send returned, so `accepted` / `rejected` / `unknown` always describe the
//! command the agent just issued. It used to poll the handle's LATCHING `last_error`, which made
//! every tool call after the first refusal return that same refusal, including commands the node
//! executed — an agent reading that would reasonably retry and place the order twice.
//!
//! **Rationale (node proto v4).** Every write tool also takes an optional `reason` string — WHY the
//! agent is issuing this command. It rides BESIDE the command
//! (`Request::Command { cmd, reason }`), is echoed back in the mandatory preview so the agent (and
//! the human watching it) sees exactly what will be recorded, and lands in the node's audit trail —
//! sanitized server-side (`vike_tradehub::audit::sanitize_reason`). It NEVER reaches the order, the
//! core fold, the journal, or a venue.
//!
//! # When the node connection drops (a tunnel going, the daemon restarting)
//!
//! The two node connections are opened lazily and held for the life of the process, and until this
//! section existed NOTHING ever let go of a dead one: `self.control = None` was assigned nowhere, so
//! after a drop every later write answered `control command not sent: Gone` until the operator
//! restarted the process — and, the SAFETY half, `node_snapshot` kept answering from the dead
//! observe handle's cell, which holds the LAST frame the node pushed with nothing marking it as
//! old. An agent asking "what are my positions" after a drop got a picture from before it, with no
//! error, and acted on it. The published remote-node setup page had to tell the reader to restart
//! the mcp process; this is what lets that instruction NARROW to the one case the paragraph after
//! the bullets names — it cannot go outright, and the reason is worth reading before editing it.
//!
//! The design is RECONNECT ON THE NEXT CALL, and nothing more: no background thread, no retry loop,
//! no timer. It needs no new state and no clock, and — the property that matters for a surface
//! trusted with money — it cannot RESEND anything, because the only thing it ever does with a dead
//! handle is drop it. Reads and writes get the two halves they need:
//!
//! - **Reads never lie.** [`Server::tool_node_snapshot`] answers ONLY from a handle whose receive
//!   thread is still alive (`RemoteCoreHandle::is_connected`). A dead one is dropped and ONE fresh
//!   connection is attempted in the same call; if that fails, the tool returns an ERROR naming the
//!   address as DOWN and the last frame's `seq` as STALE. A stale frame is never a result.
//! - **Writes can recover, and an UNKNOWN one is never silently retried.** [`Server::execute`]
//!   clears the control handle on every dead-link finding, so the next write call reconnects. What
//!   THIS call then does depends on which finding it was, and the two are now told apart
//!   structurally rather than guessed at: a command the sender proved was NEVER SENT
//!   (`CommandOutcome::NeverSent` / `ControlRejected::Gone` — not one byte on the wire) is sent on
//!   a fresh connection IN THIS CALL, because there was no first execution to double; a command
//!   whose outcome is UNKNOWN (`CommandOutcome::Disconnected` — written, reply lost) returns the
//!   same error it always did, and resending it is how one order becomes two. `Server::execute`'s
//!   doc argues the split, including which half of M11's reasoning it reverses and why.
//!
//! ⚠ **"Dropped" used to mean only a drop the SOCKET REPORTED, and that bounded both halves.**
//! `is_connected` flips when the receive thread's read returns an error — the tunnel process
//! exiting, the daemon restarting, a peer FIN or RST — and until the node grew a heartbeat there
//! was nothing else that could make it error: a subscribed observe pipe is one-way, so a link that
//! died SILENTLY (the laptop sleeping, a Wi-Fi change, a VPN re-key, with the local `ssh` client
//! holding its forwarded socket open and never sending FIN) reported nothing at all, the gate
//! passed, and `node_snapshot` answered the pre-drop frame as live FOR THE LIFE OF THIS PROCESS.
//! (This sentence used to end "until the OS keepalive gave up two hours later". There is no such
//! backstop — nothing in this workspace enables `SO_KEEPALIVE`; `vike_tradehub_client::liveness`'s
//! module doc carries the correction and the grep.)
//!
//! **That is closed in `vike-tradehub-client`, not here, and it took THREE deadlines because this
//! process holds THREE kinds of socket** — the fix is not observe-only, and reading it as
//! observe-only is how the write half stayed wedged:
//!
//! * **The observe stream** (`node_snapshot`'s pushed frames): the node writes an idle
//!   `Response::Pong` every `liveness::OBSERVE_HEARTBEAT` and `RemoteCoreHandle` deadlines its read
//!   at three of them, so a silent death ends the receive loop through its EXISTING error arm and
//!   the second pass below reconnects transparently (the publisher hands its last frame on
//!   subscribe).
//! * **The control worker's reply read** (every write tool call): deadlined at
//!   `liveness::CONTROL_REPLY_TIMEOUT`. `RemoteControlHandle`'s pre-write probe cannot see a silent
//!   death — silence peeks `WouldBlock`, which is what a healthy quiet link says — so the write
//!   went into a kernel buffer nothing would drain and the worker blocked forever. `is_connected`
//!   stayed `true`, so nothing here let the handle go, and every later write tool call queued
//!   behind the wedge and was answered `"sent": true, "outcome": "unknown"` for a command that had
//!   never left this process. Now the worker ends: the written command is `Disconnected`, whatever
//!   was queued behind it is `NeverSent`, and [`Server::execute`]'s never-sent path reconnects and
//!   sends those on the same call.
//! * **The handshake** (`preview_command` and every per-call verb open one): deadlined too, for the
//!   half-open tunnel where `TcpStream::connect` to the LOCAL ssh port succeeds and the `Welcome`
//!   read then blocked this single-threaded server forever.
//!
//! What remains for the operator is smaller but real. The observe detection window is 45 s of
//! silence, and it is armed only against a node that advertises the capability (a client that
//! deadlined a heartbeat-less node would tear down every healthy idle stream). The control window
//! BOUNDS the wedge rather than erasing it: for up to `CONTROL_REPLY_TIMEOUT` after a silent death,
//! a second write tool call is enqueued and answered "sent, outcome unknown" while it is really
//! still in the queue — finite and self-healing now, permanent before. A per-call verb's reply read
//! (`preview_command` and its siblings) is still undeadlined once the handshake hands the stream
//! back, so a link that dies inside that sub-second window still blocks this server; that residual
//! is named here rather than implied. Running the tunnel as
//! `ssh -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -L …` still tears the forwarded socket
//! down on the ssh client's own schedule, which is why the setup page keeps that line. The frame
//! carries no node-side timestamp, so there is still nothing here to AGE a frame by — the gate is
//! liveness, not freshness.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_datahub_client::DatahubClient;
use vike_model::client_order_id::ClientOrderIdGenerator;
use vike_tradehub_client::wire::WireCommand;
use vike_tradehub_client::{
    CommandOutcome, ControlRejected, RemoteControlHandle, RemoteCoreHandle,
};

use crate::cmd::args::{self, Flags};
use crate::cmd::indicators;
use crate::cmd::nodekeys::{self, NodeKeyring};
use crate::cmd::verbs;

/// The datahub address the `run_backtest` tool ships profiles to (mirrors the `backtest` command's
/// default; overridable with `--addr`).
const DEFAULT_ADDR: &str = "127.0.0.1:7878";
const SERVER_NAME: &str = "vike-cli";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The MCP protocol version we default to when a client does not declare one (we ECHO the client's
/// requested version on `initialize` when present). `2024-11-05` is the widely-supported baseline.
const DEFAULT_PROTOCOL: &str = "2024-11-05";
const USAGE: &str = "usage: vike-cli mcp [--addr 127.0.0.1:7878] [--node <host:port>]";
/// How long a write tool waits for the node's verdict on the command it just sent before reporting
/// the outcome as unknown. A DEADLINE, not a sleep — `await_outcome` returns the instant that
/// command's reply lands — so it is generous on purpose: an agent acts on this answer, and "unknown"
/// is the expensive one.
const ACK_WAIT: Duration = Duration::from_secs(2);

/// The MCP server state: the datahub address for backtests, the optional vike-tradehub node address,
/// and the two lazily-opened persistent node connections (control = write, observe = read). The
/// connections are persistent — NOT per-call — because a control command is fire-and-forget over a
/// background worker (a per-call connect could drop the socket before the command is sent) and an
/// observe connection needs to stay subscribed to keep receiving snapshot pushes.
///
/// Persistent is not the same as immortal. Each handle is held until the call that USES it finds
/// it dead — [`Server::tool_node_snapshot`] through `is_connected`, [`Server::execute`] through the
/// `Disconnected`/`Gone` outcome of a send — at which point that call sets the field back to `None`
/// and the next call reopens it through the same `ensure_*` that opened it the first time. Nothing
/// else touches the fields, deliberately: a supervisor thread would need shared state and a policy
/// for what to do with a command that was in flight, and the answer to that is always "nothing —
/// the agent decides", which is exactly what reconnect-on-next-call gives for free. (Until this
/// paragraph existed `self.control = None` was assigned nowhere, so a dropped connection was
/// dropped for the life of the process; the module doc carries the whole defect.)
///
/// ⚠ `pub` with every field PRIVATE, and the combination is the point:
/// `crates/vike-cli/tests/mcp_transcript.rs` is an integration test, so it is a separate crate and
/// can only reach [`serve`] through a public type — but it must not be able to hand this server a
/// node address or a key, because the whole property it proves is that a write never leaves the
/// machine. [`test_server`] is the only constructor it gets.
pub struct Server {
    datahub_addr: String,
    node_addr: Option<String>,
    control: Option<RemoteControlHandle>,
    observe: Option<RemoteCoreHandle>,
    /// The advisory preview caps, resolved once at startup — see [`verbs::guardrail_caps`]. The
    /// notional half is this machine's `policy.toml` ceiling (settings unification, Phase 5); the
    /// node's own `ControlLimits` is what actually enforces.
    caps: verbs::GuardrailCaps,
    /// The two node keys, resolved ONCE by the dispatcher from the process environment and then the
    /// credential store `vike-tradehub` itself reads. This used to be two `std::env::var` calls, so
    /// an agent on a box whose keys live in the store got "not set in the environment" for every
    /// live tool. See [`crate::cmd::nodekeys`].
    keys: NodeKeyring,
    /// This server's client-order-id generator — the node REFUSES a `Submit` with an empty
    /// `client_order_id`, so a tool call that omits the optional argument must still arrive with
    /// one. See [`verbs::coid_minter`].
    coids: ClientOrderIdGenerator,
    /// Previews issued and not yet confirmed. A write tool executes ONLY against a token from
    /// this store that is unexpired and bound to the SAME command — see [`PendingPreviews`].
    pending: PendingPreviews,
    /// Whatever `run_backtest` last returned IN THIS SESSION, rendered, so the
    /// `vike://backtest/last` resource has something to serve. **Per-process and deliberately so:**
    /// there is no report store in this workspace, and inventing one behind a resource read is a
    /// storage decision, not a protocol one. `None` until a backtest has run, and the resource says
    /// exactly that rather than serving an empty document.
    last_backtest: Option<String>,
}

/// How long an issued preview stays confirmable. Mirrors the Telegram surface's
/// `CONFIRM_WINDOW_MS`: long enough for an agent to read a verdict and decide, short enough that a
/// preview taken against a stale book cannot be confirmed against a moved one.
const PREVIEW_WINDOW: Duration = Duration::from_secs(60);

/// A preview that was issued and not yet consumed by a confirming call.
struct PendingPreview {
    cmd: WireCommand,
    issued: Instant,
}

/// The bounded preview-token store — single use, expiring, and BOUND to the command it previewed.
///
/// ⚠ **THE TOKEN IS NOT A SECRET, and does not need to be.** This is a stdio server: the only party
/// that can send it a request is the agent already holding both ends of the pipe, so there is no
/// third party to withhold it from. What a token proves is that a preview HAPPENED FOR THIS EXACT
/// COMMAND — it encodes a sequence, not an authorization. Guessing a token before any preview finds
/// an empty store; guessing a live one to confirm a DIFFERENT command fails the binding compare in
/// [`Server::call_tool`]. That is the whole property, and a counter delivers it.
///
/// ⚠ The store is bounded by pruning on every issue, not by a cap: an agent that previews without
/// ever confirming would otherwise grow it for the life of the process.
#[derive(Default)]
struct PendingPreviews {
    by_token: std::collections::BTreeMap<String, PendingPreview>,
    next: u64,
}

/// Do two commands express the SAME INTENT — everything the agent specified, ignoring the
/// `client_order_id`?
///
/// ⚠ The id MUST be excluded, and finding out why is what the first version of this gate got wrong:
/// [`verbs::fill_client_order_id`] mints a fresh id on EVERY call, so a preview and its confirm can
/// never carry the same one and an exact compare rejected every confirm — `submit_order` would have
/// been permanently unusable through this surface.
///
/// Excluding it costs nothing, because the confirming call does not execute the command it rebuilt:
/// it executes the STORED one. So the id that reaches the venue is the id the preview displayed and
/// the node dry-ran, which is a stronger property than comparing it would have been.
fn same_intent(a: &WireCommand, b: &WireCommand) -> bool {
    fn without_id(c: &WireCommand) -> WireCommand {
        let mut c = c.clone();
        if let WireCommand::Submit(o) = &mut c {
            o.client_order_id = String::new();
        }
        c
    }
    without_id(a) == without_id(b)
}

impl PendingPreviews {
    /// Mint a token for `cmd` and remember the binding.
    fn issue(&mut self, cmd: WireCommand) -> String {
        self.by_token.retain(|_, p| p.issued.elapsed() <= PREVIEW_WINDOW);
        self.next += 1;
        let token = format!("pv-{}", self.next);
        self.by_token.insert(token.clone(), PendingPreview { cmd, issued: Instant::now() });
        token
    }

    /// Consume a token. REMOVES before the caller executes, which is what makes it single-use even
    /// against a duplicated call. An EXPIRED entry is still returned and consumed — the caller
    /// reports the expiry distinctly, exactly as `PendingConfirms::take` does.
    fn take(&mut self, token: &str) -> Option<PendingPreview> {
        self.by_token.remove(token)
    }
}

/// Entry point the dispatcher routes to. Parses config, then serves the stdio MCP loop until EOF.
///
/// `policy_max_notional` is this machine's `max_notional_per_order` policy ceiling, already
/// resolved by [`crate::run`] (it replaced the removed `VIKE_MAX_ORDER_NOTIONAL`), and used only
/// by the mandatory preview's advisory guardrail. `keys` is the resolved [`NodeKeyring`] — same
/// story: the dispatcher owns the environment sweep and the credential-store read, this file takes
/// the answer.
///
/// # ⚠ Why this verb has NO connect/refuse rung of its own
///
/// Every other verb that opens a socket classifies the failure onto [`crate::exit`]'s ladder. This
/// one deliberately does not, and the reason is structural rather than an omission: a `mcp` process
/// is a SERVER, and its per-call failures are answers, not exits. Each of its
/// `DatahubClient::connect` / `RemoteControlHandle::connect` / `RemoteCoreHandle::connect` sites
/// returns the failure as a JSON-RPC tool error to the client that asked, which then decides what
/// to do — exiting the process on one would kill an agent's whole session over a single
/// unreachable node. The same goes for a guardrail refusal: it rides in the mandatory preview's
/// payload.
///
/// So this function has exactly two process outcomes, and both are already right. The parse error
/// goes through the shared [`args::exit_for_parse_error`] and lands on the usage rung with every
/// other verb's. The only other failure is [`serve`]'s `io::Result` — a stdio read or write that
/// broke — which is the ordinary "it ran and failed" rung by definition, and is what
/// `ExitCode::FAILURE` already spells.
pub fn run(
    args: impl Iterator<Item = String>,
    policy_max_notional: Option<f64>,
    keys: &NodeKeyring,
) -> ExitCode {
    let (datahub_addr, node_addr) = match parse_config(args) {
        Ok(c) => c,
        Err(msg) => return args::exit_for_parse_error("mcp", USAGE, &msg),
    };
    let mut server = Server {
        datahub_addr,
        node_addr,
        control: None,
        observe: None,
        caps: verbs::guardrail_caps(policy_max_notional),
        keys: keys.clone(),
        coids: verbs::coid_minter(),
        pending: PendingPreviews::default(),
        last_backtest: None,
    };
    let stdin = io::stdin();
    let stdout = io::stdout();
    match serve(stdin.lock(), stdout.lock(), &mut server) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli mcp: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A [`Server`] with NO node address and NO credentials, for tests that drive the transport.
///
/// The absent node is what makes the harness safe AND what makes it prove something: a confirm
/// that clears the preview gate fails at the CONNECTION (`no vike-tradehub node configured`),
/// which is positive evidence it got PAST the gate — while a confirm the gate refuses never
/// reaches that error at all. The two outcomes are distinguishable, and neither one can send a
/// byte to a venue.
///
/// ⚠ A second constructor, not a replacement for the inline `mod tests`' own `server()` helper —
/// that one is used by dozens of tests and stays. This one exists because `tests/` is a SEPARATE
/// CRATE and cannot build a struct whose fields are private.
pub fn test_server() -> Server {
    Server {
        datahub_addr: DEFAULT_ADDR.to_string(),
        node_addr: None,
        control: None,
        observe: None,
        caps: verbs::GuardrailCaps::default(),
        keys: NodeKeyring::default(),
        coids: verbs::coid_minter(),
        pending: PendingPreviews::default(),
        last_backtest: None,
    }
}

/// Parse `--addr <datahub>` (default [`DEFAULT_ADDR`]) and the optional `--node <host:port>` (the
/// vike-tradehub node the trade tools control), via the shared [`crate::cmd::args`] glue. A
/// `--help`/`-h` short-circuits through [`args::help_requested`]; [`args::exit_for_parse_error`] in
/// [`run`] is what turns that back into a stdout usage and an exit 0.
fn parse_config(args: impl Iterator<Item = String>) -> Result<(String, Option<String>), String> {
    let mut addr = DEFAULT_ADDR.to_string();
    let mut node: Option<String> = None;
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--addr" => addr = flags.value(&flag, inline)?,
            "--node" => node = Some(flags.value(&flag, inline)?),
            "-h" | "--help" => return args::help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok((addr, node))
}

/// The stdio loop: read newline-delimited JSON-RPC messages, dispatch each through the [`Server`],
/// write one response per request (notifications get none). A malformed line is answered with a
/// JSON-RPC parse error rather than killing the connection. Split from [`run`] so tests drive it.
///
/// ⚠ That last sentence was aspirational for a long time: every inline test called
/// [`Server::handle`] or [`Server::call_tool`] directly, so the FRAMING — the newline delimiting,
/// the `for line in reader.lines()` loop, the parse-error envelope — was exercised by nothing.
/// `crates/vike-cli/tests/mcp_transcript.rs` is the first caller that actually drives it, which is
/// why this is `pub`: an integration test is a separate crate.
pub fn serve(reader: impl BufRead, mut writer: impl Write, server: &mut Server) -> io::Result<()> {
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => server.handle(&msg),
            Err(e) => Some(rpc_error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(resp) = response {
            writeln!(writer, "{resp}")?;
            writer.flush()?;
        }
    }
    Ok(())
}

impl Server {
    /// Dispatch one JSON-RPC message, returning the response (`None` for a notification — no `id`).
    fn handle(&mut self, msg: &Value) -> Option<Value> {
        let method = msg.get("method").and_then(Value::as_str)?;
        let id = msg.get("id").cloned();
        match method {
            "initialize" => {
                let proto = msg
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(DEFAULT_PROTOCOL)
                    .to_string();
                Some(rpc_result(
                    id?,
                    json!({
                        "protocolVersion": proto,
                        // ⚠ A capability declared here is a PROMISE that the matching methods
                        // answer, and a client that reads `resources` will call `resources/list`
                        // without being asked to. Add a key only alongside its arms.
                        "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
                        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                    }),
                ))
            }
            "notifications/initialized" => None,
            "ping" => Some(rpc_result(id?, json!({}))),
            "tools/list" => Some(rpc_result(id?, json!({ "tools": tools_spec() }))),
            "resources/list" => Some(rpc_result(id?, json!({ "resources": resources_spec() }))),
            "resources/read" => {
                let uri = msg.pointer("/params/uri").and_then(Value::as_str).unwrap_or_default();
                // The same guard `tools/call` carries below, and for the same code: a resource is
                // a second WAY IN to the tool's implementation (`vike://node/snapshot` IS
                // `tool_node_snapshot`), so a panic that `tools/call` turns into an `isError`
                // result must not become a dead stdio session when the same code is reached by
                // URI. A resource has no tool-result envelope, so here it is a JSON-RPC error.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.read_resource(uri)
                }));
                match outcome {
                    // MCP wants an ARRAY of contents even for a single-document resource, and the
                    // `uri` echoed back — a client may have several reads in flight.
                    Ok(Ok(text)) => Some(rpc_result(
                        id?,
                        json!({ "contents": [{ "uri": uri, "mimeType": "application/json", "text": text }] }),
                    )),
                    // -32002 is MCP's "resource not found", and an unreachable node lands here too:
                    // a read that could not be served is an ERROR, never an empty document. An empty
                    // document is what an agent would summarise as "the node has no positions".
                    Ok(Err(e)) => Some(rpc_error(id?, -32002, &e)),
                    Err(_) => {
                        Some(rpc_error(id?, -32603, &format!("resource handler panicked: {uri}")))
                    }
                }
            }
            "prompts/list" => Some(rpc_result(id?, json!({ "prompts": prompts_spec() }))),
            "prompts/get" => {
                let name = msg.pointer("/params/name").and_then(Value::as_str).unwrap_or_default();
                let empty = json!({});
                let arguments = msg.pointer("/params/arguments").unwrap_or(&empty);
                // Guarded like `resources/read` above. `render_prompt` is pure today, but the guard
                // is a property of the SESSION, not of the handler: every arm that runs code on the
                // agent's behalf carries it, so the next arm added cannot forget.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    render_prompt(name, arguments)
                }));
                match outcome {
                    Ok(Ok(text)) => Some(rpc_result(
                        id?,
                        // A prompt is a USER turn the client pastes in — `role: "user"`, not
                        // "system": these are instructions the human is asking for, and a client
                        // that shows them to the human is doing the right thing.
                        json!({
                            "description": prompt_description(name),
                            "messages": [
                                { "role": "user", "content": { "type": "text", "text": text } }
                            ]
                        }),
                    )),
                    // -32602 (invalid params) rather than a rendered apology: an unknown prompt
                    // name is the CLIENT asking for something that does not exist, and answering
                    // it with prose would hand an agent an instruction sheet it invented the name of.
                    Ok(Err(e)) => Some(rpc_error(id?, -32602, &e)),
                    Err(_) => {
                        Some(rpc_error(id?, -32603, &format!("prompt handler panicked: {name}")))
                    }
                }
            }
            "tools/call" => {
                let id = id?;
                let name = msg.pointer("/params/name").and_then(Value::as_str).unwrap_or_default();
                let empty = json!({});
                let arguments = msg.pointer("/params/arguments").unwrap_or(&empty);
                // A panicking tool handler must not take down the stdio session (the vike-mcp
                // `rpc.rs` catch_unwind idiom): surface it as an `isError` tool result, same as
                // `Err(String)`, not a JSON-RPC protocol error and never a dead loop.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.call_tool(name, arguments)
                }));
                Some(match outcome {
                    Ok(Ok(structured)) => rpc_result(id, tool_ok(structured)),
                    Ok(Err(e)) => rpc_result(id, tool_err(&e)),
                    Err(_) => rpc_result(id, tool_err(&format!("tool panicked: {name}"))),
                })
            }
            _ => id.map(|id| rpc_error(id, -32601, &format!("method not found: {method}"))),
        }
    }

    /// Run one tool by name, returning its `structuredContent` on success or an error string
    /// (surfaced as an `isError` tool result). The order-write tools execute only when BOTH
    /// `confirm: true` AND a valid `preview_token` arrive together; either one missing returns a
    /// preview — and a preview is not network-free: with a node and a control key configured,
    /// [`Server::node_preview`] dials the node to ask what it WOULD do. What a preview never does
    /// is send the command. (This comment used to say "require `confirm: true` to execute; without
    /// it they return a preview and touch NO network" — wrong on both halves since the token
    /// landed, and the sentence the published pages were copied from.)
    fn call_tool(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        match name {
            "validate_strategy" => tool_validate_strategy(args),
            "discover_params" => tool_discover_params(args),
            "list_templates" => Ok(tool_list_templates()),
            "list_indicators" => tool_list_indicators(args),
            "run_backtest" => {
                let report = tool_run_backtest(&self.datahub_addr, args)?;
                // Remember it so `vike://backtest/last` has something to serve — the resource is a
                // second WAY IN to this answer, not a second source of it. Recorded only on
                // success: a failed run must not replace the last report that actually completed.
                self.last_backtest = Some(render(&report));
                Ok(report)
            }
            "run_sweep" => tool_run_sweep(&self.datahub_addr, args),
            "run_walk_forward" => tool_run_walk_forward(&self.datahub_addr, args),
            "list_strategies" => tool_list_strategies(&self.datahub_addr),
            "list_series" => tool_list_series(&self.datahub_addr),
            "node_snapshot" => self.tool_node_snapshot(),
            // The order-write tools: build the WireCommand (through the SHARED
            // `crate::cmd::verbs` construction site the trade REPL uses), gate on `confirm`.
            // The guard reads [`WRITE_TOOLS`] rather than re-spelling it as a seven-alternative
            // pattern, so this arm cannot drift from the roster the annotations and the transcript
            // harness read — see `the_write_arm_and_the_write_roster_are_the_same_set`.
            n if is_write_tool(n) => {
                let cmd = verbs::wire_command_for(name, args)?;
                // MINT the client-order-id when the agent did not supply one — the node REFUSES a
                // remote `Submit` with an empty `client_order_id`, so `submit_order` without the
                // optional argument used to be un-executable. Minted BEFORE the confirm branch so
                // the preview shows a real id; a `confirm: true` call mints its own (the preview
                // note says so, and `client_order_id` in the accepted response is authoritative).
                let cmd = verbs::fill_client_order_id(cmd, &mut self.coids);
                // The optional rationale is read SEPARATELY (it rides beside the command, never
                // inside it) and carried straight through to the node's audit trail.
                let reason = verbs::reason_from_tool_args(args);
                // ── THE GATE ─────────────────────────────────────────────────────────────────
                // A write executes ONLY against a `preview_token` this server issued, that is
                // unexpired, and that is BOUND to this exact command. `confirm: true` alone is no
                // longer enough, and that is the fix: it used to be, so a FIRST-and-only call with
                // `confirm: true` executed while this module's own doc promised "only a second
                // call ... sends it". Nothing correlated the two calls either — previewing
                // `qty: 0.5` and confirming `qty: 50` was accepted.
                //
                // Fails CLOSED in every direction: no token, unknown token, expired token, or a
                // token bound to a different command all return a preview or an error, never a
                // send. An agent still following the old two-call contract gets a preview.
                let token = args.get("preview_token").and_then(Value::as_str);
                let confirmed = args.get("confirm").and_then(Value::as_bool) == Some(true);
                let Some(token) = token.filter(|_| confirmed) else {
                    // MANDATORY PREVIEW: describe what WOULD happen, execute nothing — and ask the
                    // NODE what it would do, because the client-side guardrail cannot price a
                    // market order (no `price` to size) and market is the default order type.
                    let node = self.node_preview(&cmd);
                    let issued = self.pending.issue(cmd.clone());
                    return Ok(preview_of(name, &cmd, reason.as_deref(), self.caps, &issued, node));
                };
                let Some(previewed) = self.pending.take(token) else {
                    return Err(format!(
                        "preview_token {token:?} is unknown or already used — a token fires at most \
                         once. Call this tool WITHOUT `confirm` to get a fresh preview, then confirm \
                         with the `preview_token` it returns."
                    ));
                };
                if previewed.issued.elapsed() > PREVIEW_WINDOW {
                    return Err(format!(
                        "preview_token {token:?} EXPIRED (previews stay confirmable for {}s). The \
                         book may have moved since it was priced — take a fresh preview.",
                        PREVIEW_WINDOW.as_secs()
                    ));
                }
                if !same_intent(&previewed.cmd, &cmd) {
                    return Err(
                        "preview_token does not match this command — it was issued for a DIFFERENT \
                         one, and a token is bound to what it previewed. Preview the command you \
                         intend to send, then confirm with THAT token."
                            .to_string(),
                    );
                }
                // ⚠ Execute the PREVIEWED command, not the rebuilt one. They express the same
                // intent (checked above) but only the stored one carries the client_order_id the
                // preview displayed and the node dry-ran — so what reaches the venue is exactly
                // what was shown, id included.
                self.execute(&previewed.cmd, reason)
            }
            // A test-only tool that panics, pinning the catch_unwind guard in `handle` (mirrors
            // the vike-mcp `rpc.rs` fake host's "panic" tool). Never advertised in `tools_spec`.
            #[cfg(test)]
            "__test_panic" => panic!("kaboom"),
            other => Err(format!("unknown tool: {other}")),
        }
    }

    /// Serve one resource by URI.
    ///
    /// ⚠ Every arm here goes through the SAME implementation the matching tool uses — the node
    /// snapshot through [`Server::tool_node_snapshot`], the report through whatever `run_backtest`
    /// last returned. A resource is a second WAY IN, never a second source: a hand-rolled fetch
    /// beside the tool's would be free to disagree with it, and an agent reading both would have no
    /// way to tell which one was lying.
    ///
    /// `&mut self` rather than the `&self` a read suggests, because the snapshot arm opens the
    /// observe connection lazily — reading is the thing that connects.
    fn read_resource(&mut self, uri: &str) -> Result<String, String> {
        match uri {
            "vike://node/snapshot" => self.tool_node_snapshot().map(|snap| render(&snap)),
            // ABSENT is an error, not an empty document: an agent handed `{}` would summarise it as
            // a backtest that produced nothing, which is a different claim from "none has run".
            "vike://backtest/last" => self.last_backtest.clone().ok_or_else(|| {
                "no backtest has run in this session yet — call the run_backtest tool first. This \
                 resource is per-process: there is no report store, so it is empty in a fresh \
                 session even if a backtest ran in an earlier one."
                    .to_string()
            }),
            // A test-only URI that panics, pinning the catch_unwind guard on `resources/read` the
            // way `__test_panic` pins the one on `tools/call`. Never in `resources_spec`.
            #[cfg(test)]
            "vike://__test_panic" => panic!("kaboom"),
            other => Err(format!(
                "unknown resource uri: {other:?} — call resources/list for what this server serves"
            )),
        }
    }

    /// Read the live node snapshot (orders / positions / equity / recent events) via the observe
    /// connection. Opens it lazily, then waits briefly for the node's first pushed frame — and
    /// answers ONLY from a connection that is still delivering frames.
    ///
    /// ⚠ **A read must never lie, and this used to.** `RemoteCoreHandle::snapshot` is a load off
    /// the handle's arc-swap cell, and the receive loop that fills it breaks on the first read
    /// error WITHOUT clearing the cell — it only flips `is_connected` to `false`. So after the
    /// node went away (a tunnel dropping, the daemon restarting) this tool kept returning the last
    /// frame the node ever pushed: complete, well-formed, `seq` intact, and with nothing on it
    /// saying it was old. An agent asking "what are my positions" got the answer from before the
    /// drop, with no error, and acted on it. That is the safety defect this method now closes.
    ///
    /// The shape: exactly TWO passes over "ensure a handle, wait for a frame, check it is live".
    /// The first pass runs on whatever handle is held; if that one is dead it is dropped and the
    /// second pass opens ONE fresh connection in this same call, so a tunnel that has come back is
    /// transparent to the agent. If the fresh connection cannot be opened, or drops again before
    /// a frame is read, the answer is an ERROR naming the address as DOWN and the last frame's
    /// `seq` as STALE — never that frame as a result. The next call tries again from scratch.
    /// There is no third pass and no sleep between the two: a read is cheap to repeat and the
    /// agent is told it may.
    ///
    /// The liveness check is deliberately AFTER the wait, not before it, so there is ONE gate and
    /// it covers the whole read: a handle that was alive when this call started and died while
    /// the call waited for its first frame would otherwise answer with the `seq: 0` placeholder as
    /// though the node had said "nothing". (A frame's AGE is not reported: the handle records no
    /// receipt time, and inventing one here from the call's own clock would be a number about this
    /// process, not about the node.)
    ///
    /// The stale `seq` is named on the call that FINDS the handle dead, and on that call only: the
    /// last frame lives in the handle, and the handle is let go of. A later call while the node is
    /// still down answers with [`Server::ensure_observe`]'s own connect error — still never a
    /// frame — rather than a remembered number, because remembering it would be state carried
    /// across calls, which this design deliberately has none of. Within the call, only a REAL
    /// frame's `seq` is ever named: a handle found dead while holding the `seq: 0` placeholder
    /// (opened, never delivered a frame, dropped) has no stale frame, and the error says "no frame
    /// was received" rather than calling the placeholder one. (The second pass used to assign
    /// `snap.seq` unconditionally, so a tunnel that came back and went again before its first
    /// frame reported "seq 0 is STALE" — losing the first pass's real number in exactly the
    /// flapping case the number was written for.)
    ///
    /// ⚠ **The gate is exactly as good as `is_connected`, and that bit got wider.** It flips when
    /// the receive thread's read ERRORS — a drop the TCP stack delivers (the tunnel process
    /// exiting, the daemon restarting, a FIN or RST) and, since the node grew an idle heartbeat,
    /// also when NOTHING arrives for three beats. So a link that dies without a packet (the laptop
    /// sleeping under an `ssh -L` with no `ServerAliveInterval`) no longer leaves this gate passing
    /// the pre-drop frame through as live for hours: it is caught within
    /// `vike_tradehub_client::liveness::OBSERVE_READ_TIMEOUT` and the second pass reconnects.
    /// The residuals, stated so the green is read at its width: that window is 45 s, the deadline
    /// is armed only against a node advertising the capability (a mixed-version deployment gets the
    /// old width, silently), and a frame's AGE is still not reported — the handle records no
    /// receipt time and the frame carries no node-side timestamp, so this remains a LIVENESS gate,
    /// never a freshness one.
    fn tool_node_snapshot(&mut self) -> Result<Value, String> {
        // Whether a pass has found a held handle dead — the fact that turns a plain connect error
        // into the DOWN error — and the `seq` of the last REAL frame a dead handle was holding,
        // carried into that error so the agent can tell WHICH earlier answer it must not act on.
        // The two are separate on purpose: a handle found dead on the `seq: 0` placeholder held
        // no frame, and a pass that saw a real frame must not be overwritten by a later one that
        // saw none. Before a handle has been found dead, `ensure_observe`'s own connect error is
        // returned unchanged: a node that was never reached has no stale frame to warn about.
        let mut found_dead = false;
        let mut stale_seq: Option<u64> = None;
        for _pass in 0..2 {
            if let Err(e) = self.ensure_observe() {
                return Err(if found_dead { self.observe_down(stale_seq, &e) } else { e });
            }
            let observe = self.observe.as_ref().expect("ensured");
            // A freshly-subscribed connection returns the empty placeholder (seq 0) until the node
            // pushes its first coalesced frame; wait up to ~2s for a real one.
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut snap = observe.snapshot();
            while snap.seq == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
                snap = observe.snapshot();
            }
            // ── THE GATE ─────────────────────────────────────────────────────────────────────
            // The cell is only evidence of the node's state while the thread filling it is alive.
            if observe.is_connected() {
                // Serialize the WireSnapshot as-is (it is already a flat, serde projection).
                return serde_json::to_value(&*snap)
                    .map_err(|e| format!("cannot serialize snapshot: {e}"));
            }
            // Dead: remember what it held — if it held anything — let go of it, and let the second
            // pass open a fresh one. Dropping the handle joins its receive thread; nothing is sent
            // in either direction.
            found_dead = true;
            if snap.seq > 0 {
                stale_seq = Some(snap.seq);
            }
            self.observe = None;
        }
        Err(self.observe_down(
            stale_seq,
            "the reopened connection dropped again before a frame was read",
        ))
    }

    /// The `node_snapshot` error for a node that is DOWN: names the address, the reason this call
    /// could not get a live frame, and the `seq` of the last frame this process received — flagged
    /// STALE so the agent knows WHICH earlier answer it must not act on — and says the next call
    /// will try again, so the agent neither retries in a loop nor gives up on the session.
    /// `None` is a dead handle that never held a frame (the `seq: 0` placeholder), and is worded
    /// as exactly that rather than as "seq 0 is stale": a frame that was never received cannot be
    /// the earlier answer the agent is being told to distrust.
    fn observe_down(&self, stale_seq: Option<u64>, why: &str) -> String {
        let addr = self.node_addr.as_deref().unwrap_or("<no node configured>");
        let stale = match stale_seq {
            Some(seq) => format!(
                "The last frame this session received (seq {seq}) is STALE and is deliberately \
                 NOT returned"
            ),
            None => "No frame was received on the dropped connection, so there is no stale frame \
                     to name — any earlier node_snapshot result is older still"
                .to_string(),
        };
        format!(
            "the observe connection to {addr} is DOWN ({why}). {stale}: do not act on any earlier \
             node_snapshot result — orders and positions may have changed since it was pushed. The \
             next node_snapshot call will try to reconnect again."
        )
    }

    /// Send an already-built [`WireCommand`] to the node's control connection (opened lazily) and
    /// report what the node did with THAT command — [`RemoteControlHandle::await_outcome`] on the
    /// ticket the send returned. The order's own progress (fills) is still observed via
    /// `node_snapshot`; this answers only "did the node accept it".
    /// `reason` is the optional agent rationale, carried BESIDE the command for the node's audit
    /// trail (sanitized server-side) — it is never folded into the order.
    ///
    /// ⚠ This used to poll `RemoteControlHandle::last_error`, a LATCH that is never cleared: after
    /// one refusal every later write tool in the session returned `isError: node rejected the
    /// command`, including commands the node accepted and EXECUTED. An agent reading that would
    /// reasonably RETRY — placing the order twice.
    ///
    /// # A dropped connection is let go of HERE, and only here — and a NEVER-SENT one is re-sent
    ///
    /// Three outcomes mean the control connection is dead, and they split TWO ways, which is the
    /// whole design:
    ///
    /// - **Never sent** — [`CommandOutcome::NeverSent`] (the sender found the link already closed
    ///   and wrote nothing) and [`ControlRejected::Gone`] (the worker had already exited, so
    ///   nothing was even enqueued). This call drops the handle, dials ONCE through the same
    ///   [`Server::ensure_control`] that opened the first connection, and SENDS THIS COMMAND on the
    ///   fresh one — reporting whatever the node then says. There is no double-execution risk to
    ///   weigh: not one byte of it reached the node, so there was no first execution to double. If
    ///   the reconnect fails, the answer is the connect error wrapped in a "not sent" that says so
    ///   and the handle is cleared, exactly as before; a second never-sent on the fresh link is
    ///   reported and not chased, because a third dial inside one tool call is a retry loop an
    ///   agent is waiting on.
    /// - **Unknown** — [`CommandOutcome::Disconnected`]: the command WAS written and its reply was
    ///   lost. The handle is dropped so the next call reconnects, and this call returns the same
    ///   UNKNOWN error, word for word, that it always has. It is never resent under any wording.
    ///
    /// ⚠ **This reverses half of what this doc said at M11, and the reversal is worth reading.**
    /// The old paragraph refused to re-send even a provably-unsent command, arguing that "the
    /// property that matters is not 'was this byte sent twice' but 'what picture is the agent
    /// writing against': a write issued straight after a drop is a write against a snapshot from
    /// BEFORE the drop". That argument was correct about the RISK and wrong about the remedy here,
    /// for a reason M11 could not see because the distinction did not exist yet:
    ///
    /// 1. It could not tell a never-sent command from an unknown one, so it had to treat every
    ///    dead link as the dangerous case. `vike-tradehub-client` now makes that distinction
    ///    STRUCTURALLY (a peer FIN observed BEFORE the write), and it is conservative: an ambiguous
    ///    failed write stays `Disconnected`.
    /// 2. The picture is not stale in this path. A write tool here reaches `execute` only after a
    ///    MANDATORY node-verified preview (`Server::call_tool`'s binding token check), and a
    ///    preview opens its own fresh connection per call — so the node dry-ran THIS command,
    ///    seconds ago, on a live link. And the node re-evaluates every command on execute
    ///    (`ControlLimits` + the core `RiskGate`) whichever connection it arrives on.
    /// 3. What the old behaviour cost was not caution but a dead end: the agent was handed an
    ///    error for a command that demonstrably had not happened, and the only way forward was a
    ///    `node_snapshot` round trip and a fresh preview+confirm — three tool calls to re-send a
    ///    command nothing had objected to.
    ///
    /// # The routine trigger USED to be the node's own idle timer
    ///
    /// `crates/vike-tradehub/src/server.rs`'s `HANDSHAKE_READ_TIMEOUT` (five minutes) was set on
    /// every accepted socket and never replaced, and a CONTROL peer never subscribes — it stays in
    /// the node's request/response read loop — so the node closed it the moment it had been quiet
    /// that long, and this arm reported `Disconnected` ("may have executed") for a command the
    /// node's thread had stopped reading before it was offered. Every pause longer than five
    /// minutes produced that transcript. BOTH halves of it are now fixed, in the two crates that
    /// owned them: the node replaces that bound at `AuthOk`
    /// (`vike_tradehub_client::liveness::AUTHED_IDLE_TIMEOUT`), so the close does not happen at
    /// all; and when a close does happen for some other reason (a daemon restart, a tunnel blip),
    /// the sender's pre-write probe reports it as never-sent and this call recovers on the spot.
    ///
    /// # Preview tokens survive the drop, and that is right
    ///
    /// [`PendingPreviews`] is per-PROCESS, not per-connection, and a reconnect leaves it alone: a
    /// token minted before the drop still confirms after it, as long as it is within
    /// [`PREVIEW_WINDOW`]. That is correct because of what a token PROVES — that a preview happened
    /// for this exact command in this session (the binding compare in [`Server::call_tool`]) — and
    /// what it does not: the node's dry-run verdict was never a reservation, and the node
    /// re-evaluates every command on execute (`ControlLimits` and the core `RiskGate`) whichever
    /// connection it arrives on. The window is what bounds a preview taken against a book that has
    /// since moved, and a drop does not make the book move any faster than the clock does.
    fn execute(&mut self, cmd: &WireCommand, reason: Option<String>) -> Result<Value, String> {
        // PASS 1, on whatever handle is held.
        let never_sent = match self.offer_command(cmd, reason.clone()) {
            Ok(answered) => return answered,
            Err(why) => why,
        };

        // The node never saw it. Let the dead handle go and dial ONCE — the same
        // `ensure_control` that opened the first connection, so there is no second way to reach
        // the node.
        self.control = None;
        if let Err(connect_err) = self.ensure_control() {
            self.control = None;
            return Err(format!(
                "control command not sent: {never_sent} Reconnecting to send it failed, so \
                 NOTHING was sent for this call and nothing was retried: {connect_err}. The next \
                 write tool call will try to reconnect again; call node_snapshot first to see the \
                 node's real state."
            ));
        }

        // PASS 2 on the fresh connection. A second never-sent is reported, never chased: two dead
        // links in one call is a node that is going away, and a third dial would be a retry loop
        // inside a tool call an agent is waiting on.
        match self.offer_command(cmd, reason) {
            Ok(answered) => answered,
            Err(why) => {
                self.control = None;
                Err(format!(
                    "control command not sent: {why} The reconnected control connection died \
                     too, so nothing was sent for this call and nothing was retried. Call \
                     node_snapshot before trying again."
                ))
            }
        }
    }

    /// Offer ONE command to the held control connection and resolve its ticket.
    ///
    /// `Ok(answer)` is the tool's answer for a command the node ANSWERED (or one whose outcome is
    /// unknown, which is an answer about the command). `Err(why)` is the NEVER-SENT case and only
    /// that: not one byte of this command reached the node, `why` being the sentence
    /// [`Server::execute`] embeds in whichever message it ends up returning. The split exists so
    /// the reconnect decision is made in exactly one place, over a distinction the client crate
    /// makes structurally rather than one this crate infers from an error string.
    ///
    /// The handle is dropped HERE on every dead-link finding — `Disconnected`, `NeverSent`, `Gone`
    /// — so the caller never has to remember to, and a `Refused`/timeout leaves it held.
    fn offer_command(
        &mut self,
        cmd: &WireCommand,
        reason: Option<String>,
    ) -> Result<Result<Value, String>, String> {
        // A connect failure is the TOOL's answer (M11's wording, unchanged), never a never-sent:
        // there is no held connection to let go of and nothing for a reconnect to fix — the dial
        // just failed.
        if let Err(e) = self.ensure_control() {
            return Ok(Err(e));
        }
        let addr = self.node_addr.clone().unwrap_or_default();
        let control = self.control.as_ref().expect("ensured");
        let ticket = match control.try_command_with_reason(cmd.clone(), reason) {
            Ok(ticket) => ticket,
            // The worker had already exited: the queue is closed, so NOTHING was enqueued and
            // nothing went on the wire for this call — a never-sent, and the caller may send it.
            Err(ControlRejected::Gone) => {
                self.control = None;
                return Err(format!(
                    "the control connection to {addr} had already dropped, so nothing was \
                     enqueued and nothing went on the wire."
                ));
            }
            Err(e) => return Ok(Err(format!("control command not sent: {e:?}"))),
        };
        let outcome = control.await_outcome(ticket, ACK_WAIT);
        Ok(match outcome {
            Some(CommandOutcome::Accepted { coid }) => Ok(json!({
                "sent": true,
                "outcome": "accepted",
                "client_order_id": coid,
                "note": "the node ACCEPTED this command (empty client_order_id = an account-wide verb); call node_snapshot to observe the result. The node's server-side ControlLimits + RiskGate are the enforcing gate."
            })),
            Some(CommandOutcome::Refused(err)) => Err(format!("node rejected the command: {err}")),
            // NEVER SENT: the link was already dead when the worker reached this command, so it
            // was not written at all. Hand the caller the reason rather than an answer — there is
            // nothing here for an agent to act on, and everything for this process to fix itself.
            Some(CommandOutcome::NeverSent) => {
                self.control = None;
                return Err(format!(
                    "the control connection to {addr} was already closed when this command \
                     reached the sender, so NOT ONE BYTE of it went on the wire."
                ));
            }
            // A dropped connection is NOT a refusal and NOT a success — do not let an agent infer
            // either. Say the outcome is unknown and point at the read tool that settles it. The
            // handle is discarded so the NEXT call reconnects; this command is never resent.
            Some(CommandOutcome::Disconnected) => {
                self.control = None;
                Err(format!(
                    "the control connection to {addr} dropped before the node answered this \
                     command — its outcome is UNKNOWN and it may have executed. Call node_snapshot \
                     to check BEFORE retrying. The dead connection has been discarded and the next \
                     write tool call reconnects; this command was NOT resent."
                ))
            }
            None => Ok(json!({
                "sent": true,
                "outcome": "unknown",
                "note": format!("the node did not answer within {}s — this command's outcome is NOT known and it may still execute. Call node_snapshot to check BEFORE retrying.", ACK_WAIT.as_secs())
            })),
        })
    }

    /// Open the control (write) connection if not already open. Errors if `--node` was not given or
    /// the control key is absent from the process env.
    ///
    /// "Already open" means a handle is HELD, not that it is alive: this returns early on a dead
    /// handle too, and does not probe `is_connected`. Liveness is the SENDER's finding — it is
    /// learnt from the outcome of a command that was actually offered, never from a probe before
    /// the send ([`Server::execute`]'s doc argues why a silent probe-and-reconnect was considered
    /// and refused).
    ///
    /// ⚠ This paragraph used to say [`Server::execute`] was "the ONE place that sets the field back
    /// to `None`", and that "not already open" therefore meant "found dead by the PREVIOUS write".
    /// Both halves are stale, and the second one names exactly the behaviour the never-sent
    /// recovery replaced. [`Server::offer_command`] is the place that lets a dead handle go — on
    /// `ControlRejected::Gone`, `CommandOutcome::NeverSent` and `CommandOutcome::Disconnected` —
    /// and [`Server::execute`] clears it around its two passes as well. So this is now reached
    /// TWICE per `execute` in the recovering case: once to open (or reuse) the handle for pass 1,
    /// and again for pass 2 after a never-sent finding dropped it, which is how a command the node
    /// never saw is sent on THIS call instead of the next one.
    fn ensure_control(&mut self) -> Result<(), String> {
        if self.control.is_some() {
            return Ok(());
        }
        let addr = self.node_addr.as_ref().ok_or(
            "no vike-tradehub node configured — pass `--node <host:port>` to enable order-write tools",
        )?;
        let (key, _) =
            self.keys.control().ok_or_else(|| self.missing_key(nodekeys::CONTROL_KEY_ENV))?;
        let handle = RemoteControlHandle::connect(addr.as_str(), key.as_bytes())
            .map_err(|e| format!("cannot open control connection to {addr}: {e}"))?;
        self.control = Some(handle);
        Ok(())
    }

    /// Ask the NODE what it would do with `cmd`, without sending it.
    ///
    /// ⚠ THIS IS THE ONLY VERDICT THAT MEANS ANYTHING FOR A MARKET ORDER, and market is the
    /// DEFAULT order type. The client-side [`verbs::guardrail_check`] prices an order as
    /// `price * qty`, and a market order carries no `price` — so its notional is `None` and the cap
    /// silently does not apply. The node knows the mark, holds the real `ControlLimits` and runs
    /// the same `RiskGate` a live send would hit.
    ///
    /// Returns `None` when there is no node configured, no control key, or the node could not be
    /// reached — the caller then LABELS the preview as an unverified client-side estimate rather
    /// than presenting an absent verdict as an approving one.
    fn node_preview(&self, cmd: &WireCommand) -> Option<Value> {
        let addr = self.node_addr.as_ref()?;
        let (key, _) = self.keys.control()?;
        match vike_tradehub_client::preview_command(addr.as_str(), key.as_bytes(), cmd) {
            Ok((accepted, reason)) => Some(json!({
                "checked_by": CHECKED_BY_NODE,
                "accepted": accepted,
                "reason": reason,
            })),
            // A handshake or transport fault is NOT a verdict. Say which it was and keep the
            // preview honest about being unverified — this arm is `Some` so the preview can NAME
            // the fault it hit, and [`preview_of`] reads `checked_by` (never `is_some()`) to tell
            // it apart from a verdict.
            Err(e) => Some(json!({
                "checked_by": CHECKED_BY_NONE,
                "accepted": Value::Null,
                "reason": format!("the node could not be asked: {e}"),
            })),
        }
    }

    /// The "this key is nowhere" tool error. It names BOTH sources, because the old message said
    /// "is not set in the environment" while the daemon's own keys sat unread in the credential
    /// store — an agent (and the human reading its transcript) has no way to diagnose that.
    fn missing_key(&self, name: &str) -> String {
        format!(
            "{name} is set neither in the process environment nor in the credential store — \
             run `vike-cli secrets path` to see which store this project resolves to, and \
             `vike-cli secrets list` to see whether the key is in it"
        )
    }

    /// Open the observe (read) connection if not already open. Errors if `--node`/observe key absent.
    ///
    /// Same rule as [`Server::ensure_control`]: a HELD handle is returned as-is, alive or not. The
    /// liveness gate is [`Server::tool_node_snapshot`]'s, the one reader, which drops a dead handle
    /// and calls this again in the same call — so the "fresh connection" a reconnect opens is the
    /// same code path as the first connection, with no second way to dial the node.
    fn ensure_observe(&mut self) -> Result<(), String> {
        if self.observe.is_some() {
            return Ok(());
        }
        let addr = self.node_addr.as_ref().ok_or(
            "no vike-tradehub node configured — pass `--node <host:port>` to read node state",
        )?;
        let (key, _) =
            self.keys.observe().ok_or_else(|| self.missing_key(nodekeys::OBSERVE_KEY_ENV))?;
        let handle = RemoteCoreHandle::connect(addr.as_str(), key.as_bytes())
            .map_err(|e| format!("cannot open observe connection to {addr}: {e}"))?;
        self.observe = Some(handle);
        Ok(())
    }
}

// ---- pure tool implementations (no `self` / no network) --------------------------------------

/// Compile-check a Rhai strategy OFFLINE (the absorbed `vike-mcp` `validate_strategy` tool —
/// compile IS validation). Goes through `vike_script::discover_params`, the broker-free public
/// compile path: it compiles the script and runs its top level exactly once — the SAME error
/// surface as mounting the strategy (`RhaiStrategy::compile` does the same one-time top-level
/// run). A bad script is an `ok:false` ANSWER (the agent reads the error and fixes the script),
/// never a tool error; only a missing argument is.
fn tool_validate_strategy(args: &Value) -> Result<Value, String> {
    let script = args
        .get("script")
        .and_then(Value::as_str)
        .ok_or("validate_strategy requires a `script` string argument")?;
    Ok(match vike_script::discover_params(script) {
        Ok(_) => json!({ "ok": true }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    })
}

/// The starter Rhai strategies, as `{name, code}` rows — the absorbed `vike-mcp` `list_templates`
/// tool. The sources are STATIC DATA copied verbatim from `vike_studio_core::templates` (importing
/// that crate would drag DataFusion into this deliberately DataFusion-free CLI); each is
/// parameterized via `param()` so it drops straight into `run_sweep`, and each is pinned by two
/// tests below — one that COMPILES it, and one that EXECUTES it over a broker double and asserts
/// an order arrives. The second exists because an agent COPIES this list, and a script whose
/// every call sits inside `fn on_bar()` compiles perfectly while failing on every single bar.
fn tool_list_templates() -> Value {
    let templates: Vec<Value> =
        TEMPLATES.iter().map(|(name, code)| json!({ "name": name, "code": code })).collect();
    json!({ "templates": templates })
}

/// `(name, source)` starters (verbatim from `vike-studio-core/src/templates.rs`).
///
/// ⚠ That crate is NOT imported (it would drag DataFusion into this deliberately DataFusion-free
/// CLI), so these are a HAND COPY with no cross-crate drift gate — the two must be edited together.
/// Each side carries its own EXECUTION gate over its own copy instead:
/// `every_shipped_template_reaches_the_broker` below, and
/// `vike-studio-core/tests/templates_execute.rs`.
const TEMPLATES: &[(&str, &str)] =
    &[("SMA cross", SMA_CROSS), ("RSI reversion", RSI_REVERSION), ("Donchian breakout", BREAKOUT)];

const SMA_CROSS: &str = r#"
let fast = param("fast", 5.0);
let slow = param("slow", 20.0);
fn on_bar() {
    let f = sma(fast.to_int()); let s = sma(slow.to_int());
    if s.is_nan() { return; }
    let target = if f > s { 1.0 } else { -1.0 };
    let delta = target - position();
    if abs(delta) > 1e-12 { market(if delta > 0.0 { 1 } else { -1 }, abs(delta)); }
}
"#;

const RSI_REVERSION: &str = r#"
let len = param("len", 14.0);
let lo = param("lo", 30.0);
let hi = param("hi", 70.0);
fn on_bar() {
    let r = rsi(len.to_int());
    if r.is_nan() { return; }
    if r < lo && position() <= 0.0 { market(1, 1.0); }
    if r > hi && position() >= 0.0 { market(-1, 1.0); }
}
"#;

const BREAKOUT: &str = r#"
let len = param("len", 20.0);
fn on_bar() {
    let h = sma(len.to_int());   // symmetric proxy. donchian is NOT bound: it is multi-output
                                 // (upper/mid/lower) and the bridge binds single-output indicators
                                 // only, so `donchian(len)` is a function-not-found at the first
                                 // call rather than a silent upper-band read.
    if h.is_nan() { return; }
    if high() > h && position() <= 0.0 { market(1, 1.0); }
    if low() < h && position() >= 0.0 { market(-1, 1.0); }
}
"#;

fn tool_discover_params(args: &Value) -> Result<Value, String> {
    let script = args
        .get("script")
        .and_then(Value::as_str)
        .ok_or("discover_params requires a `script` string argument")?;
    let params =
        vike_script::discover_params(script).map_err(|e| format!("rhai compile error: {e}"))?;
    let params: Vec<Value> = params
        .into_iter()
        .map(|(name, default)| json!({ "name": name, "default": default }))
        .collect();
    Ok(json!({ "params": params }))
}

/// The HOST-BOUND callable set — `vike_script::is_callable`, what
/// `crates/vike-script/src/engine.rs`'s `register_indicators` actually registers under a bare name
/// OR a per-line accessor (`bollinger` has no bare call and three accessors) — joined onto its
/// `vike_indicators::registry()` metadata. NEVER the registry itself: a script calling a name the
/// host does not bind hits a function-not-found error every bar and self-disables, so a roster that
/// over-advertises hands an agent names that produce a strategy which looks mounted and never
/// trades. The filter, the rows and the human `vike-cli indicators` listing are all
/// `crates/vike-cli/src/cmd/indicators.rs`'s (`bound_metas`/`detail_row`/`compact_row`), so the two
/// surfaces cannot disagree about what is callable.
///
/// # ⚠ TIERED, because this roster is now long
///
/// The bound set was three names when this tool was written and is the near-whole catalog now, so
/// a single full response is no longer small: a full row carries a label, a category, every
/// parameter with its default and the value that comes back, and an agent that asked "what can I
/// call" would be handed tens of kilobytes — most of it about indicators it will not use — inside
/// its context window.
///
/// So the DEFAULT answer is the compact roster (name + category, the two fields that make a list
/// navigable), and detail is pulled per `name` or per `category`. That is the only size decision
/// this file makes: the `tools/list` DESCRIPTION deliberately names none of the set (see
/// [`tools_spec`]), because that text ships in every session whether the tool is called or not.
fn tool_list_indicators(args: &Value) -> Result<Value, String> {
    let name = args.get("name").and_then(Value::as_str).filter(|s| !s.trim().is_empty());
    let category = args.get("category").and_then(Value::as_str).filter(|s| !s.trim().is_empty());
    if let Some(name) = name {
        let m = indicators::find_bound(name)?;
        return Ok(json!({ "indicators": [indicators::detail_row(m)], "count": 1 }));
    }
    let all = indicators::bound_metas();
    if let Some(category) = category {
        let rows = indicators::filter_by_category(&all, category)?;
        let payload: Vec<Value> = rows.iter().map(|m| indicators::detail_row(m)).collect();
        return Ok(json!({ "indicators": payload, "count": payload.len() }));
    }
    let payload: Vec<Value> = all.iter().map(|m| indicators::compact_row(m)).collect();
    Ok(json!({
        "indicators": payload,
        "count": payload.len(),
        "note": "the callable roster, compact. Call again with `name` for one indicator in full (its parameters with their defaults, and the value it returns) or with `category` for a whole family. A name absent from this list is NOT callable: rhai resolves a function name when the line runs, so a script naming it compiles and then fails on every bar until the strategy switches itself off. `name` on such a name answers with the reason it is held back."
    }))
}

fn tool_run_backtest(datahub_addr: &str, args: &Value) -> Result<Value, String> {
    let profile = args
        .get("profile")
        .and_then(Value::as_str)
        .ok_or("run_backtest requires a `profile` string argument")?;
    let mut profile_toml = profile.to_string();
    if let Some(script) = args.get("script").and_then(Value::as_str) {
        profile_toml = crate::cmd::backtest::inject_script_src(&profile_toml, script)?;
    }
    let mut client = DatahubClient::connect(datahub_addr)
        .map_err(|e| format!("cannot connect to datahub at {datahub_addr}: {e}"))?;
    let report_json = client.run_backtest(&profile_toml)?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server report was not valid JSON: {e}"))?;
    Ok(json!({ "report": report }))
}

/// List the compiled NATIVE backtest-strategy roster the remote vike-datahub server advertises
/// (`vike_backtest::harness::STRATEGIES` over RPC) — the names a profile's `strategy.name` can
/// resolve, so an agent can discover which strategies exist before authoring a profile. Connects
/// to the datahub like `run_backtest`; a connect failure or a server-side error is a clean tool
/// error.
fn tool_list_strategies(datahub_addr: &str) -> Result<Value, String> {
    let mut client = DatahubClient::connect(datahub_addr)
        .map_err(|e| format!("cannot connect to datahub at {datahub_addr}: {e}"))?;
    let strategies = client.list_strategies()?;
    Ok(json!({ "strategies": strategies }))
}

/// Resolve the profile TOML a run tool ships from its arguments: the required `profile` string,
/// with the optional `script` injected as `[strategy.params].src` (the `run_backtest`/`backtest
/// --script` idiom). Returned as TEXT — the run tools ship it verbatim, exactly like the
/// subcommands, so the SERVER's `BacktestProfile::from_toml_str` is the only profile parser.
fn profile_from_args(args: &Value, tool: &str) -> Result<String, String> {
    let profile = args.get("profile").and_then(Value::as_str).ok_or_else(|| {
        format!("{tool} requires a `profile` string argument (a backtest profile TOML)")
    })?;
    let mut profile_toml = profile.to_string();
    if let Some(script) = args.get("script").and_then(Value::as_str) {
        profile_toml = crate::cmd::backtest::inject_script_src(&profile_toml, script)?;
    }
    Ok(profile_toml)
}

/// Run a remote parameter-grid sweep on the vike-datahub server — the absorbed `vike-mcp`
/// `run_sweep`, gone remote: EXACTLY the request the `sweep` subcommand builds (the SAME profile
/// TOML over the SAME `run_sweep_profile` wire verb), so the tool and the subcommand can never
/// drift. The optional `rank_by` argument names the server-side metric.
///
/// NOTE the trade this shares with `run_backtest`: shipping the TOML verbatim means a profile-SHAPE
/// error (no `[sweep]` table, bad range, unknown strategy) now surfaces from the SERVER rather than
/// before the connect. One parser, one error source.
fn tool_run_sweep(datahub_addr: &str, args: &Value) -> Result<Value, String> {
    let profile_toml = profile_from_args(args, "run_sweep")?;
    let rank_by = args.get("rank_by").and_then(Value::as_str);
    let mut client = DatahubClient::connect(datahub_addr)
        .map_err(|e| format!("cannot connect to datahub at {datahub_addr}: {e}"))?;
    let report_json = client.run_sweep_profile(&profile_toml, rank_by)?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server sweep report was not valid JSON: {e}"))?;
    Ok(json!({ "sweep": report }))
}

/// Run a remote anchored out-of-sample walk-forward on the vike-datahub server — the absorbed
/// `vike-mcp` `run_walk_forward`, gone remote: the SAME request the `walkforward` subcommand builds
/// (the profile TOML over the `run_walkforward_profile` wire verb). The split count rides IN the
/// profile's `[walkforward]` table.
fn tool_run_walk_forward(datahub_addr: &str, args: &Value) -> Result<Value, String> {
    let profile_toml = profile_from_args(args, "run_walk_forward")?;
    let mut client = DatahubClient::connect(datahub_addr)
        .map_err(|e| format!("cannot connect to datahub at {datahub_addr}: {e}"))?;
    let report_json = client.run_walkforward_profile(&profile_toml)?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server walk-forward report was not valid JSON: {e}"))?;
    Ok(json!({ "walkforward": report }))
}

/// List every stored series the remote vike-datahub server holds, with its cheap coverage — the
/// absorbed `vike-mcp` `list_series`, gone remote over the datahub `Inventory` metadata verb (the
/// coverage-carrying superset of `ListSeries`, so an agent can pick a `[data]` range, not just a
/// name). Rows are `{kind, venue, symbol, interval, first_ts, last_ts, rows}`.
fn tool_list_series(datahub_addr: &str) -> Result<Value, String> {
    let mut client = DatahubClient::connect(datahub_addr)
        .map_err(|e| format!("cannot connect to datahub at {datahub_addr}: {e}"))?;
    let inventory = client.inventory()?;
    let series: Vec<Value> = inventory
        .into_iter()
        .map(|(id, cov)| {
            json!({
                "kind": id.kind, "venue": id.venue, "symbol": id.symbol, "interval": id.interval,
                "first_ts": cov.first_ts, "last_ts": cov.last_ts, "rows": cov.rows,
            })
        })
        .collect();
    Ok(json!({ "series": series }))
}

/// The two `node_verdict.checked_by` values, spelled ONCE. [`Server::node_preview`] writes one of
/// them and [`preview_of`] reads it back to decide whether the preview was verified — a question
/// that must be asked of the VERDICT, never of the `Option` wrapping it.
const CHECKED_BY_NODE: &str = "node";
/// The node was reached for, and did not answer — a transport fault, a denied handshake, or the
/// client-side refusal. See [`CHECKED_BY_NODE`].
const CHECKED_BY_NONE: &str = "none";

/// The mandatory-preview payload for a write tool: the resolved command, the SHARED client-side
/// guardrail check ([`crate::cmd::verbs::guardrail_check`] — advisory; the node's server-side gate
/// is the enforcing one), the rationale that will be recorded, and the confirm hint. PURE.
///
/// `reason` is ECHOED here on purpose: it is the one part of the request the agent cannot otherwise
/// see the effect of (it goes to the node's audit trail, not to the order), so the preview shows it
/// alongside the command it will be filed against. It is echoed as SENT — the node applies its own
/// sanitization (`vike_tradehub::audit::sanitize_reason`) before recording.
fn preview_of(
    name: &str,
    cmd: &WireCommand,
    reason: Option<&str>,
    caps: verbs::GuardrailCaps,
    preview_token: &str,
    node: Option<Value>,
) -> Value {
    let wire = serde_json::to_value(cmd).unwrap_or(Value::Null);
    // ⚠ Whether the node ANSWERED decides the wording, and the wording is the point. An absent
    // verdict must never read as an approving one: the client-side guardrail cannot price a market
    // order (no `price` to size against) and market is the DEFAULT order type, so on that path the
    // local check is vacuous rather than permissive-by-accident.
    //
    // ⚠ The question is asked of the VERDICT, not of the `Option`. [`Server::node_preview`] returns
    // `Some` on its FAILURE path too — a `checked_by: "none"` row carrying the transport error, so
    // the preview can name the fault rather than silently dropping the field — and `node.is_some()`
    // therefore reported `verified_by_node: true` for a node that was dialled and refused, or
    // unreachable, or that answered `AuthDenied`. That is the one flag the prompts this server
    // serves teach an agent to read as "the verdict that counts", so it must mean asked AND
    // answered. `a_node_that_could_not_be_asked_is_not_a_verified_preview` is the pin on the pure
    // payload; `a_preview_whose_node_could_not_be_asked_is_not_verified` pins the same property
    // end to end, through `handle` against a node that refuses the connection.
    let verified = node.as_ref().is_some_and(|v| v["checked_by"] == CHECKED_BY_NODE);
    json!({
        "will_execute": false,
        "tool": name,
        "wire_command": wire,
        "guardrail": verbs::guardrail_check(cmd, caps).to_json(),
        "node_verdict": node,
        "verified_by_node": verified,
        "preview_token": preview_token,
        "reason": reason,
        "note": if verified {
            "PREVIEW ONLY — nothing was sent. `node_verdict` is the NODE's own dry-run against the \
             real ControlLimits + RiskGate; it is the verdict that counts. To execute, call again \
             with BOTH \"confirm\": true AND this exact \"preview_token\". The token fires once, \
             expires after 60s, and is bound to THIS command — confirming a different command with \
             it is refused. For submit_order, `wire_command.Submit.client_order_id` was minted here \
             and is the id that will be sent. Any `reason` is recorded in the node's audit trail \
             (sanitized) and never reaches the order."
        } else {
            "PREVIEW ONLY — nothing was sent. ⚠ THE NODE WAS NOT ASKED (no node configured, no \
             control key, or unreachable), so `guardrail` is an UNVERIFIED CLIENT-SIDE ESTIMATE — \
             and it cannot size a market order at all, because a market order carries no price. Do \
             not read it as approval. To execute, call again with BOTH \"confirm\": true AND this \
             exact \"preview_token\". The token fires once, expires after 60s, and is bound to THIS \
             command. The node's ControlLimits + RiskGate remain the enforcing gate regardless."
        }
    })
}

/// The two-call gate, defined ONCE and spliced into every write tool — so the wording cannot drift
/// between them, which is how the old inline copies ended up in four different spellings.
fn confirm_property() -> Value {
    json!({
        "type": "boolean",
        "description": "must be true AND accompanied by a valid `preview_token` to execute; either one missing returns a preview instead"
    })
}

/// The token half of the gate. See [`PendingPreviews`] for why it is not a secret.
fn preview_token_property() -> Value {
    json!({
        "type": "string",
        "description": "the `preview_token` returned by this tool's preview call. Fires ONCE, expires after 60s, and is BOUND to the exact command it previewed — confirming a different command with it is refused."
    })
}

/// The optional `reason` property every WRITE tool advertises — one definition, spliced into each
/// tool's `inputSchema` so the wording (and the fact that it is never required) can never drift
/// between tools.
fn reason_property() -> Value {
    json!({
        "type": "string",
        "description": "optional rationale — WHY this command is being issued. Recorded in the node's audit trail (control characters stripped, capped at 512 chars); never reaches the order, the core, or the venue."
    })
}

/// The MCP write-tool roster — the FULL `trade`-REPL write-verb set (the drift the shared
/// [`crate::cmd::verbs`] module exists to prevent). Module scope rather than `#[cfg(test)]`
/// because three consumers now need it outside the inline test module: the [`Server::call_tool`]
/// routing, the registry manifest gate, and `crates/vike-cli/tests/mcp_transcript.rs`.
///
/// ⚠ `the_write_arm_and_the_write_roster_are_the_same_set` is the THIRD EDGE of the triangle. The
/// `destructiveHint` annotations in [`tools_spec`] were already held equal to this list; the
/// ROUTING was held equal to nothing — it was a seven-alternative pattern, and a tool added to one
/// spelling and not the other is either an ungated write or an unexercised gate.
pub const WRITE_TOOLS: [&str; 7] = [
    "submit_order",
    "cancel_order",
    "modify",
    "flatten",
    "market_exit",
    "set_trading_state",
    "mass_cancel",
];

/// Does this tool go through the mandatory-preview gate? The one predicate the routing and the
/// annotations both read, so neither can answer differently.
///
/// `pub(crate)` and not `pub`, unlike [`WRITE_TOOLS`] beside it: the transcript harness is a
/// separate crate and needs the ROSTER, but it drives tools by name over the transport rather than
/// asking this question, so nothing outside vike-cli calls it and widening the crate's public API
/// by an item nobody consumes buys nothing.
pub(crate) fn is_write_tool(name: &str) -> bool {
    WRITE_TOOLS.contains(&name)
}

/// The `resources/list` payload.
///
/// A resource is a READ the agent does not have to choose a tool for: the same data
/// `node_snapshot` and `run_backtest` return, addressable by URI so a client can pin it into a
/// conversation (or re-read it) without a tool call. Both are DERIVED — [`Server::read_resource`]
/// routes each one into the tool's own implementation, so a resource can never answer differently
/// from the tool beside it.
///
/// ⚠ Neither is a write, and no resource ever will be. `resources/read` has no preview gate and no
/// `confirm` argument, because there is nothing here to confirm; a URI that changed node state
/// would be a write with the whole gate routed around it.
fn resources_spec() -> Value {
    json!([
        {
            "uri": "vike://node/snapshot",
            "name": "Node snapshot",
            "description": "The live vike-tradehub node's orders, positions, per-venue equity and recent events — the same payload the node_snapshot tool returns. Requires --node + VIKE_TRADEHUB_OBSERVE_KEY; reading it is what opens the connection.",
            "mimeType": "application/json"
        },
        {
            "uri": "vike://backtest/last",
            "name": "Last backtest report",
            "description": "Whatever the run_backtest tool last returned IN THIS SESSION. Per-process: there is no report store, so a fresh session serves an error rather than an older run's report.",
            "mimeType": "application/json"
        }
    ])
}

/// The one rendering of a structured payload into the `text` a client displays — shared by
/// [`tool_ok`] and [`Server::read_resource`] so a resource and its tool cannot present the same
/// document differently.
fn render(structured: &Value) -> String {
    serde_json::to_string_pretty(structured).unwrap_or_default()
}

// ---- prompts ---------------------------------------------------------------------------------

/// The two-call gate, spelled ONCE for the prompts that describe it.
///
/// ⚠ Written from [`Server::call_tool`]'s own behaviour, deliberately not from any published page:
/// the pages describing this surface said `confirm: true` was the whole gate, which stopped being
/// true when the token binding landed — so a prompt copied from one would teach an agent to be
/// refused on its second call and to have no idea why. Every clause below is a branch you can read
/// in that function.
///
/// ⚠ A FUNCTION rather than a `const`, and that is the whole point: the roster comes from
/// [`WRITE_TOOLS`] and the window from [`PREVIEW_WINDOW`], because this is the ONE document in the
/// tree that teaches an agent WHICH tools need two calls. Written out by hand it was a fourth
/// spelling of the roster held equal to nothing — so an eighth write tool would join the routing,
/// the `destructiveHint` annotations and the transcript harness automatically and be missing from
/// exactly the sheet an agent reads before calling it once and believing it executed.
/// `every_write_touching_prompt_teaches_the_token_not_just_confirm` walks the roster over the
/// rendered text, so a re-hardcoding is caught as well as prevented.
fn two_call_gate() -> String {
    format!(
        "\
Every order-write tool ({roster}) takes TWO calls, and the first one sends nothing:

1. Call the tool with its arguments and NO `confirm`. You get back `will_execute: false`, the \
resolved `wire_command`, a `guardrail` estimate, a `node_verdict`, and a `preview_token`.
2. READ THE VERDICT BEFORE CONFIRMING. `verified_by_node: true` means the node itself dry-ran the \
command against the real ControlLimits and RiskGate — that is the verdict that counts. \
`verified_by_node: false` means NO verdict came back: either the node was never asked (none \
configured, no control key) or it was asked and did not answer (unreachable, handshake refused, \
or it returned an error). `node_verdict.reason` says which. In that case `guardrail` is an \
unverified client-side estimate; it cannot price a MARKET order at all, because a market order \
carries no price, and market is the default order type. Do not read an absent verdict as approval.
3. Call the SAME tool again with the same arguments plus BOTH `confirm: true` AND the exact \
`preview_token` from step 1.

`confirm: true` on its own is not a confirmation — it returns another preview. A token fires at \
most ONCE, expires {secs} seconds after it was issued, and is BOUND to the command it previewed: \
confirming different arguments with it is refused, so preview the command you actually intend to \
send. (The one field excluded from that binding is `client_order_id`, because this server mints a \
fresh one per call; the order that reaches the venue carries the id the preview displayed.)

The optional `reason` argument on every write tool is recorded in the node's audit trail and never \
reaches the order, the core or the venue. Fill it in.",
        roster = WRITE_TOOLS.join(", "),
        secs = PREVIEW_WINDOW.as_secs(),
    )
}

/// The `prompts/list` payload — `(name, description, arguments)` per flow.
///
/// Three flows, because these are the three an agent is asked for and gets wrong differently: a
/// backtest is a loop it can run alone, arming a venue is a decision it must NOT take alone, and a
/// stuck order is the case where reading before acting matters most.
fn prompts_spec() -> Value {
    json!([
        {
            "name": "backtest_a_strategy",
            "description": PROMPT_BACKTEST_DESC,
            "arguments": [
                { "name": "idea", "description": "the strategy idea in one sentence, if you have one", "required": false }
            ]
        },
        {
            "name": "arm_a_venue",
            "description": PROMPT_ARM_DESC,
            "arguments": [
                { "name": "venue", "description": "the venue id, e.g. binance / bybit / okx / hyperliquid", "required": false }
            ]
        },
        {
            "name": "triage_a_stuck_order",
            "description": PROMPT_TRIAGE_DESC,
            "arguments": [
                { "name": "client_order_id", "description": "the coid of the order in question, if known", "required": false }
            ]
        }
    ])
}

const PROMPT_BACKTEST_DESC: &str = "Author, validate and backtest a Rhai strategy end to end, using only the offline tools and \
     the remote datahub — no node, no orders.";
const PROMPT_ARM_DESC: &str = "Understand what actually arms a venue for live trading, and what this agent surface can and \
     cannot do about it.";
const PROMPT_TRIAGE_DESC: &str = "Diagnose an order that is not behaving — read the node's state first, then act through the \
     two-call preview gate.";

/// The `description` a `prompts/get` echoes back, held equal to the roster's by construction.
fn prompt_description(name: &str) -> Value {
    match name {
        "backtest_a_strategy" => json!(PROMPT_BACKTEST_DESC),
        "arm_a_venue" => json!(PROMPT_ARM_DESC),
        "triage_a_stuck_order" => json!(PROMPT_TRIAGE_DESC),
        _ => Value::Null,
    }
}

/// Render one prompt's instruction text. PURE — a prompt describes a flow, it does not run one.
///
/// Arguments are all OPTIONAL: an agent that calls `prompts/get` with a name alone gets a usable
/// sheet with the specifics left as placeholders, which is the shape a human browsing a client's
/// prompt menu actually gets.
fn render_prompt(name: &str, args: &Value) -> Result<String, String> {
    let arg =
        |key: &str| args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    // Rendered once for the arms that splice it — the roster and the window it names are DERIVED
    // (see [`two_call_gate`]), which is why it is a value here rather than a `const` in scope.
    let gate = two_call_gate();
    match name {
        "backtest_a_strategy" => {
            let idea = arg("idea").unwrap_or("the strategy idea you were given");
            Ok(format!(
                "Backtest {idea}, in this order. Every tool here is offline or read-only; none of \
                 it can place an order.

1. `list_indicators` with no arguments for the compact roster of what a Rhai strategy can \
                 actually CALL, then again with `name` or `category` for the ones you want in \
                 full. ⚠ This is NOT the whole indicator registry: a script calling a name the host \
                 does not bind compiles fine and then fails on EVERY bar, so the strategy looks \
                 mounted and never trades. If a name you want is missing, ask `list_indicators` \
                 for it by `name` — the answer says why it is held back.
2. `list_templates` for starter strategies. Each is parameterised with `param(name, default)` so \
                 it drops straight into a sweep.
3. Write the strategy, then `validate_strategy`. A compile error comes back as `ok: false` with \
                 the message, not as a tool error — read it and fix the script.
4. `discover_params` to see the knobs the script declares, so the profile's `[strategy.params]` \
                 and any `[sweep]` grid name keys that exist.
5. `list_series` for what data the datahub actually holds — `{{kind, venue, symbol, interval, \
                 first_ts, last_ts, rows}}` — and pick a `[data]` range inside the coverage it \
                 reports. `list_strategies` if you would rather use a compiled native strategy \
                 than a script.
6. `run_backtest` with the profile TOML and the script. The report is also readable afterwards as \
                 the `vike://backtest/last` resource, for the rest of this session only.
7. If the result looks good, do NOT stop there. `run_sweep` shows whether it survives its own \
                 parameter grid, and `run_walk_forward` shows whether it survives out of sample. A \
                 single in-sample backtest is the weakest evidence this server can produce.

Report the numbers you got, including the bad ones."
            ))
        }
        "arm_a_venue" => {
            // A placeholder rather than a phrase, because it is spliced into a `policy.toml` KEY
            // below as well as into prose — `policy.venues.the venue` would read as a real key.
            let venue = arg("venue").unwrap_or("<venue>");
            Ok(format!(
                "You have been asked about arming {venue} for live trading. Read this before \
                 doing anything.

⚠ THIS AGENT SURFACE CANNOT ARM A VENUE, and that is deliberate. Arming is a file edit and a \
                 credential decision a human makes; there is no tool here that does it, and there \
                 should not be.

What actually decides, in the order the mount consults it:

1. `policy.venues.{venue}` in `<project>/settings/policy.toml` — `paper` | `demo` | `live`, \
                 defaulting to `paper` for every venue. It is a CEILING and can only ever REFUSE: \
                 `live` arms nothing by itself, it merely declines to stop the venue. A box with \
                 no `[venues]` table mounts ALL PAPER whatever its credential store holds, and \
                 says so once at startup.
2. The credentials, read only if the ceiling allowed it. Absent credentials ARE the live gate — \
                 the venue stays on the paper simulator. `vike-cli secrets path` prints which \
                 store this project resolves to and `vike-cli secrets list` prints the key NAMES \
                 in it (never the values).

So the human's job is: raise the ceiling for that one venue in `policy.toml`, and put the right \
                 key set in the store. Yours is to tell them which of the two is missing.

What you CAN do here, once someone else has armed it:
- `node_snapshot` (or the `vike://node/snapshot` resource) to read what the node is actually doing.
- `set_trading_state` to move the account between `active` / `reducing` / `halted`. ⚠ That is the \
                 KILL SWITCH, not an arming control — it is a write tool and goes through the \
                 gate below like any other.

{gate}"
            ))
        }
        "triage_a_stuck_order" => {
            let which = arg("client_order_id")
                .map(|c| format!("the order with client_order_id {c:?}"))
                .unwrap_or_else(|| "an order that is not behaving".to_string());
            Ok(format!(
                "Triage {which}. READ FIRST — every step below that changes anything is a write \
                 tool behind the two-call gate.

1. `node_snapshot` (or read the `vike://node/snapshot` resource). Find the order in the live \
                 order list and look at its state, its filled quantity and the recent events. \
                 Report what you SEE before proposing anything.
2. Decide which of these it actually is, because they need opposite responses:
   - RESTING and simply not filling — the price is away from the market. `modify` its price or \
                 `cancel_order` it. Nothing is wrong.
   - GONE from the node but believed live at the venue, or present at the venue and not on the \
                 node — that is a reconciliation divergence, not a stuck order. Do NOT paper over \
                 it by submitting a replacement: you would double the position. Report it.
   - Its outcome came back UNKNOWN (the node did not answer in time, or the control connection \
                 dropped). ⚠ The command MAY HAVE EXECUTED. Read `node_snapshot` again BEFORE \
                 retrying anything — a retry here is how one order becomes two. A dropped \
                 connection is reopened by your next call; nothing needs restarting, and \
                 `node_snapshot` errors rather than answering from a stale frame while it is down. \
                 EXPECTED after any pause longer than five minutes: the node closes an idle \
                 control connection, and the first write after the pause answers UNKNOWN even \
                 though the node had stopped reading before it was sent — read `node_snapshot`, \
                 preview again, confirm again; that is a timer, not a fault. ⚠ Only a drop the \
                 socket REPORTS is detected: a link that died silently (a sleeping laptop, an ssh \
                 tunnel without ServerAliveInterval) leaves `node_snapshot` answering its last \
                 frame as live. If the picture never changes while the node should be trading, \
                 say so and have the operator check the tunnel rather than trusting it.
3. Only then act, one command at a time, re-reading `node_snapshot` between them. Put your \
                 reasoning in each write tool's `reason` argument; it lands in the node's audit \
                 trail.

⚠ `market_exit` cancels every live order and flattens every position. It is the panic button, not \
                 a triage step. Do not reach for it because one order is confusing.

{gate}"
            ))
        }
        // Test-only, pinning the `prompts/get` guard — see `vike://__test_panic` on the resource
        // side. Never in `prompts_spec`.
        #[cfg(test)]
        "__test_panic" => panic!("kaboom"),
        other => Err(format!(
            "unknown prompt: {other:?} — call prompts/list for what this server serves"
        )),
    }
}

/// The `tools/list` payload — each tool's schema + risk annotations.
fn tools_spec() -> Value {
    json!([
        {
            "name": "validate_strategy",
            "description": "Compile a Rhai strategy; returns {ok, error?}. Compile IS validation. Offline — no server.",
            "inputSchema": { "type": "object", "properties": { "script": { "type": "string", "description": "Rhai strategy source" } }, "required": ["script"] },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "discover_params",
            "description": "Discover the tunable param(name, default) knobs an authored Rhai strategy declares. Offline — no server.",
            "inputSchema": { "type": "object", "properties": { "script": { "type": "string" } }, "required": ["script"] },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "list_templates",
            "description": "List starter Rhai strategies as {name, code} — each parameterized via param() so it drops straight into run_sweep. Offline — no server.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "list_indicators",
            // ⚠ This text ships in EVERY session's tools/list, called or not, so it names none of
            // the roster — see `list_indicators_description_does_not_enumerate_the_set`. It used
            // to splice `RHAI_INDICATORS.join(", ")` in, which was 13 characters when the host
            // bound three names and is kilobytes now that it binds the catalog.
            "description": "List the HOST-BOUND indicators a Rhai strategy can actually call — what the vike-script host registers, which is not every vike-indicators registry name; a script calling an unbound one compiles and then fails on every bar. No arguments returns the compact roster (name + category). Pass `category` for one family in full, or `name` for one indicator in full — its parameters with defaults and its output line — which also answers WHY a registry name is not callable. The roster is not repeated in this text on purpose: it is long, and a description is sent whether you call the tool or not.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "one indicator, in full (its parameters with defaults, and the value it returns) — or, for a registry name the host holds back, the reason" },
                    "category": { "type": "string", "description": "one family, in full — the `category` value the roster reports (case-insensitive)" }
                }
            },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "run_backtest",
            "description": "Run a backtest on the remote vike-datahub server from a profile TOML, optionally injecting a Rhai script. Returns the BacktestReport JSON.",
            "inputSchema": {
                "type": "object",
                "properties": { "profile": { "type": "string" }, "script": { "type": "string" } },
                "required": ["profile"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "run_sweep",
            "description": "Run a parameter-grid sweep on the remote vike-datahub server from a profile TOML with a [sweep] table (each key overrides strategy.params.<key> across a value grid), optionally injecting a Rhai script. Returns the server-ranked SweepReport JSON (one row per grid point, each with its BacktestReport, best first).",
            "inputSchema": {
                "type": "object",
                "properties": { "profile": { "type": "string", "description": "a backtest profile TOML with [data]/[strategy]/[sweep] (+ optional [engine])" }, "script": { "type": "string", "description": "optional Rhai source injected as [strategy.params].src" }, "rank_by": { "type": "string", "description": "sharpe (default) | return | max_dd | equity — applied server-side" } },
                "required": ["profile"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "run_walk_forward",
            "description": "Run an anchored out-of-sample walk-forward on the remote vike-datahub server from a profile TOML with a [walkforward] table (n_splits), optionally injecting a Rhai script. Returns the stitched WalkForwardReport JSON (OOS windows + oos_sharpe + wf_consistency).",
            "inputSchema": {
                "type": "object",
                "properties": { "profile": { "type": "string", "description": "a backtest profile TOML with [data]/[strategy]/[walkforward] (+ optional [engine])" }, "script": { "type": "string", "description": "optional Rhai source injected as [strategy.params].src" } },
                "required": ["profile"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "list_strategies",
            "description": "List the compiled native backtest strategies the remote vike-datahub server offers (the names a profile's strategy.name can resolve). Requires a reachable datahub (--addr).",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        },
        {
            "name": "list_series",
            "description": "List every stored data series the remote vike-datahub server holds, with coverage: {kind, venue, symbol, interval, first_ts, last_ts, rows} — what a profile's [data] table can name. Requires a reachable datahub (--addr).",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        },
        {
            "name": "node_snapshot",
            "description": "Read the running vike-tradehub node's live state (orders, positions, per-venue equity, recent events). Requires --node + VIKE_TRADEHUB_OBSERVE_KEY. If the node connection has dropped — a drop the socket REPORTS: the tunnel process exiting, the daemon restarting — this returns an ERROR naming it DOWN and the last frame STALE, never that frame as if live, and reconnects on the next call, so call it again rather than restarting anything. ⚠ A link that died SILENTLY (a sleeping laptop, an ssh tunnel without ServerAliveInterval) is NOT detected: the last frame is answered as live until the socket reports the drop, and the frame carries no timestamp to age it by. If the picture never changes while the node should be trading, have the operator check the tunnel before trusting it.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "submit_order",
            "description": "Submit an order on the vike-tradehub node. WITHOUT confirm:true this only PREVIEWS (executes nothing). Requires --node + VIKE_TRADEHUB_CONTROL_KEY.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "venue": { "type": "string" }, "symbol": { "type": "string" },
                    "side": { "type": "integer", "description": "1 buy / -1 sell" },
                    "qty": { "type": "number" },
                    "order_type": { "type": "string", "description": "market | limit | stop | take_profit (default market)" },
                    "price": { "type": "number" }, "trigger_price": { "type": "number" },
                    "reduce_only": { "type": "boolean" },
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                },
                "required": ["venue", "symbol", "side", "qty"]
            },
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        },
        {
            "name": "cancel_order",
            "description": "Cancel one resting order by client_order_id. WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": {
                "type": "object",
                "properties": { "client_order_id": { "type": "string" }, "reason": reason_property(), "confirm": confirm_property(), "preview_token": preview_token_property() },
                "required": ["client_order_id"]
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "modify",
            "description": "Modify one resting order's qty and/or price by client_order_id (at least one of new_qty/new_price). PREVIEW IS MANDATORY: WITHOUT confirm:true this only PREVIEWS (executes nothing).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "client_order_id": { "type": "string" },
                    "new_qty": { "type": "number" }, "new_price": { "type": "number" },
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                },
                "required": ["client_order_id"]
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "flatten",
            "description": "Close the (venue, symbol) net position with a reduce-only market order. WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": {
                "type": "object",
                "properties": { "venue": { "type": "string" }, "symbol": { "type": "string" }, "reason": reason_property(), "confirm": confirm_property(), "preview_token": preview_token_property() },
                "required": ["venue", "symbol"]
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "market_exit",
            "description": "PANIC BUTTON — cancel every live order then flatten every position (optionally scoped to one venue). WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": {
                "type": "object",
                "properties": { "venue": { "type": "string", "description": "omit for every engine" }, "reason": reason_property(), "confirm": confirm_property(), "preview_token": preview_token_property() }
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "mass_cancel",
            "description": "Cancel EVERY live order, optionally scoped to one venue and/or symbol. PREVIEW IS MANDATORY: WITHOUT confirm:true this only PREVIEWS (executes nothing).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "venue": { "type": "string", "description": "omit for every venue" },
                    "symbol": { "type": "string", "description": "omit for every symbol" },
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                }
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "set_trading_state",
            "description": "Set the account trading state / kill switch. WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": {
                "type": "object",
                "properties": { "state": { "type": "string", "description": "active | reducing | halted" }, "reason": reason_property(), "confirm": confirm_property(), "preview_token": preview_token_property() },
                "required": ["state"]
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        }
    ])
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_ok(structured: Value) -> Value {
    let text = render(&structured);
    json!({ "content": [ { "type": "text", "text": text } ], "structuredContent": structured, "isError": false })
}

fn tool_err(message: &str) -> Value {
    json!({ "content": [ { "type": "text", "text": message } ], "isError": true })
}

// The NODE-DROP suite — its own file because it stands up a real paper node behind a loopback
// relay the test owns (a harness, not one more case for the inline module below), and a child
// module rather than an integration test because it must reach `Server`'s private fields:
// `Server`'s doc argues that no PUBLIC constructor may take a node address or a key.
// ⚠ `#[path]` because the file sits BESIDE `mcp.rs` rather than under it — a non-root module
// looks in a subdirectory named after itself. `crates/vike-tradehub/src/tradehub_cli.rs`'s
// `feed_splice_seam_tests` is the precedent, and the precedent's ONE structural rule is that the
// module NAME equals the file STEM: the src-walking gates (`crates/vike-ops/tests/clock_pin.rs`'s
// `cfg_test_module_files`, lifted into `system_temp_gate.rs` and `compile_time_path_gate.rs`)
// derive a `#[cfg(test)] mod NAME;` file module as `{dir}/NAME.rs` and read no `#[path]`, so this
// module was `node_drop_tests` for one commit and resolved a file that does not exist — leaving
// the real one classified as LIBRARY code: green while it reads no env, no temp dir and no clock,
// and a misleading red the day one of those joins it. The `#[cfg(test)]` line also has to sit
// directly above the `mod` line, because that is the pair the derivation reads.
#[path = "mcp_node_drop_tests.rs"]
#[cfg(test)]
mod mcp_node_drop_tests;

#[cfg(test)]
mod tests {
    use super::*;

    // The EXECUTION gate on the shipped templates (see `every_shipped_template_reaches_the_broker`):
    // the real `RhaiStrategy` mounted over vike-model's shared `MockBroker` (its `test-support`
    // feature, already a dev-dep of this crate for `tests/init_cli.rs`) and driven over real bars.
    use vike_model::strategy::MockBroker;
    use vike_model::{Bar, Strategy};
    use vike_script::RhaiStrategy;

    /// The same no-node, no-credential server `crates/vike-cli/tests/mcp_transcript.rs` drives,
    /// reached through the public constructor rather than re-spelled — a second field list here is
    /// a second place to forget a field, and the two would then disagree about what "a fresh
    /// server" is.
    fn server() -> Server {
        test_server()
    }

    fn req(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn call(name: &str, args: Value) -> Value {
        server().handle(&req(1, "tools/call", json!({ "name": name, "arguments": args }))).unwrap()
    }

    #[test]
    fn initialize_echoes_protocol_and_names_the_server() {
        let resp = server()
            .handle(&req(1, "initialize", json!({ "protocolVersion": "2025-06-18" })))
            .unwrap();
        assert_eq!(resp["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(resp["result"]["serverInfo"]["name"], "vike-cli");
    }

    /// A function name NOTHING binds — the specimen the non-vacuity proof below rewrites a template
    /// to call. Typo-shaped on purpose: the failure it stands for is a misspelling, and the two
    /// assertions at the call site prove it is unbound rather than trusting this comment.
    const UNBOUND_WITNESS: &str = "sma_typo";

    #[test]
    fn the_write_arm_and_the_write_roster_are_the_same_set() {
        // The `call_tool` match arm decides what is GATED; `WRITE_TOOLS` decides what is
        // ADVERTISED as destructive and what the transcript harness exercises. A tool in one and
        // not the other is either an ungated write or an unexercised gate — and until this test
        // existed the arm was held equal to NOTHING: the annotations and the roster were pinned to
        // each other, the routing was a seven-alternative pattern nobody compared to either.
        for name in WRITE_TOOLS {
            assert!(is_write_tool(name), "{name} is in WRITE_TOOLS but not routed as a write");
        }
        let spec = tools_spec();
        for tool in spec.as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let destructive = tool["annotations"]["destructiveHint"].as_bool() == Some(true);
            assert_eq!(
                destructive,
                is_write_tool(name),
                "{name}: destructiveHint and the write routing disagree"
            );
        }
    }

    #[test]
    fn the_registry_manifest_lists_every_tool_this_server_serves() {
        // The manifest is what a registry user reads BEFORE installing. A tool added to
        // `tools_spec` and not to the manifest is a promise the listing does not make; one in the
        // manifest and not the server is a promise it cannot keep. Neither is visible to any other
        // gate — the manifest is a hand-written file at the repository root, and nothing else in
        // this workspace reads it.
        //
        // ⚠ It compares `_vike.tools`, a block WE own, rather than anything the upstream registry
        // schema names. The schema has already changed more than once; a gate keyed on an upstream
        // field would silently stop checking the day a key was renamed.
        const MANIFEST: &str = include_str!("../../../../server.json");
        let manifest: Value = serde_json::from_str(MANIFEST).expect("server.json is valid JSON");
        let listed: Vec<&str> = manifest["_vike"]["tools"]
            .as_array()
            .expect("server.json carries _vike.tools")
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        let spec = tools_spec();
        let served: Vec<&str> =
            spec.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(listed, served, "server.json's tool list has drifted from tools_spec");
        assert_eq!(
            manifest["version"].as_str(),
            Some(SERVER_VERSION),
            "server.json's version must match the crate the server reports at initialize"
        );
        // The registry's `description` is capped at 100 characters (the 2025-12-11 schema), and a
        // manifest that overruns it is refused at publish time rather than here — which is the
        // wrong place to find out, since publishing is a deliberate one-off act.
        let description =
            manifest["description"].as_str().expect("server.json carries description");
        assert!(
            (1..=100).contains(&description.chars().count()),
            "the registry caps `description` at 100 characters; this one is {}",
            description.chars().count()
        );
        // The mirror's FORBID scan aborts the WHOLE publish on a box name or a private path, and
        // this file is the first root-level manifest to ship. Catch it here, where the failure
        // names the manifest, rather than at release time where it names a grep.
        assert!(
            !MANIFEST.contains("the latency box") && !MANIFEST.contains("the CI box"),
            "server.json must name no host — the mirror's FORBID scan aborts the publish"
        );
    }

    /// **THE MCP SURFACE ADVERTISES NO CREDENTIAL WRITER, AND CONTAINS NO CALL INTO ONE.**
    ///
    /// `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` is the
    /// record, and its fourth reason is the one this test holds: `mcp` is a verb of the SAME binary
    /// as `secrets`, one dispatch arm away from any CLI writer, so the only way to keep a credential
    /// write out of the agent surface for certain is to keep it out of this file. That mattered in
    /// the abstract while `vike-cli` had no writer at all; since `vike-cli secrets set` exists it is
    /// a live property, and the record's reopen clause names an MCP tool as one of the four things
    /// that would re-decide the whole verdict.
    ///
    /// The comparison is against Hummingbot's MCP server, whose `setup_connector` tool takes an
    /// AGENT-SUPPLIED credentials dict and writes it, gated only by a `confirm_override` flag for a
    /// connector that already exists — so a NEW key is written with no gate at all. A credential
    /// write is a larger grant than an order, and this server already makes order preview mandatory.
    ///
    /// ⚠ Two evidences, because either alone is defeatable. A tool NAMED `rotate_secret` with an
    /// innocuous description passes a description-only check; a tool called `configure` that
    /// happened to call the writer passes a name-only one. So: no advertised tool may pair a
    /// credential word with a write word, AND this file may not call the writer at all.
    ///
    /// ⚠ **What it does NOT walk: the PROMPT texts.** The subject/act scan reads `tools_spec` only,
    /// and the prompt bodies (`arm_a_venue`) plus a tool error string (`missing_key`) do pair a
    /// credential word with a store word — they would trip this test if it were pointed at them,
    /// which is why the scope is stated rather than left to be inferred from a green run. It is a
    /// deliberate bound and not an oversight: a prompt EXECUTES nothing, and the second evidence
    /// below — this file calls no writer — already holds for the whole file, prompts included. The
    /// residual is a prompt that TELLS an agent to reach a credential by another route (a shell
    /// tool, say), which `0036`'s reopen clause explicitly covers ("including one that merely calls
    /// a shell") and which no text scan of this file could catch anyway.
    #[test]
    fn the_mcp_surface_advertises_no_credential_writer() {
        // The subject words, and the act words. Substring matching on purpose — `secrets`,
        // `credential` and `rotate_secret` all have to be caught, and a tool that talks about a
        // credential without proposing to change one (there are none today) would still be flagged
        // and would then be a deliberate decision rather than a drift.
        const SUBJECT: [&str; 4] = ["secret", "credential", "api key", "api_key"];
        const ACT: [&str; 6] = ["set", "write", "rotate", "store", "save", "configure"];

        let spec = tools_spec();
        for tool in spec.as_array().expect("tools_spec is an array") {
            let name = tool["name"].as_str().expect("every tool is named").to_lowercase();
            let description = tool["description"].as_str().unwrap_or_default().to_lowercase();
            for text in [&name, &description] {
                let subject = SUBJECT.iter().find(|s| text.contains(**s));
                let act = ACT.iter().find(|a| text.contains(**a));
                if let (Some(s), Some(a)) = (subject, act) {
                    panic!(
                        "tool `{name}` pairs `{s}` with `{a}`: the MCP surface may advertise no \
                         credential write. See docs/decisions/0036 — an MCP tool is one of the \
                         four things its reopen clause says re-decides the record from the top."
                    );
                }
            }
        }

        // …and the ROUTING half: this file calls neither the workspace's one upsert nor its
        // journalled wrapper, so no tool can reach a credential write by any name at all.
        //
        // ⚠ Both needles are `concat!`ed rather than spelled, and that is not decoration: this test
        // reads its OWN file, so a whole spelling written here as test DATA would report itself as a
        // call site and the assertion could never pass. It is the same self-scanning trap
        // `crates/vike-ops/src/scan.rs`'s `find_calls` documents for the gate that walks it.
        const THIS_FILE: &str = include_str!("mcp.rs");
        const UPSERT: &str = concat!("save_", "credentials(");
        const JOURNALLED: &str = concat!("save_", "credentials_journalled(");
        for writer in [UPSERT, JOURNALLED] {
            assert!(
                !THIS_FILE.contains(writer),
                "cmd/mcp.rs calls `{writer}` — the MCP surface is read-only about credentials"
            );
        }
        // Non-vacuity: the needle really does find a call when one is present, so a rename of the
        // writer cannot turn this half silently green. `crates/vike-ops/tests/
        // credential_writer_gate.rs` is the tree-wide version of the same question, with a kill
        // proof that plants a real call.
        assert!(
            include_str!("secrets.rs").contains(UPSERT),
            "the needle must match the CLI's one writer call site, or this proves nothing"
        );
    }

    #[test]
    fn the_server_serves_prompts_and_each_one_renders() {
        let mut s = server();
        let init = s.handle(&req(0, "initialize", json!({}))).unwrap();
        assert!(
            init["result"]["capabilities"]["prompts"].is_object(),
            "initialize must declare the prompts capability once prompts/list answers"
        );
        let listed = s.handle(&req(1, "prompts/list", json!({}))).unwrap();
        let names: Vec<&str> = listed["result"]["prompts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["backtest_a_strategy", "arm_a_venue", "triage_a_stuck_order"]);
        for name in names {
            // With NO arguments — the shape a human browsing a client's prompt menu gets. Every
            // argument this server declares is optional, so this must render rather than error.
            let got = s.handle(&req(2, "prompts/get", json!({ "name": name }))).unwrap();
            let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
            assert!(!text.is_empty(), "{name} rendered an empty prompt");
            assert_eq!(got["result"]["messages"][0]["role"], "user", "{name}");
            assert_eq!(
                got["result"]["description"],
                prompt_description(name),
                "{name}: prompts/get must echo the description prompts/list advertised"
            );
        }
        let bogus = s.handle(&req(3, "prompts/get", json!({ "name": "nope" }))).unwrap();
        assert_eq!(bogus["error"]["code"], -32602, "got {bogus}");
    }

    #[test]
    fn every_write_touching_prompt_teaches_the_token_not_just_confirm() {
        // ⚠ THE FAILURE THIS PINS. The published pages describing this surface said `confirm: true`
        // was the whole gate — true before the token binding landed, false since. A prompt written
        // from one of those pages teaches an agent a flow that is REFUSED on its second call, with
        // a message about a token the prompt never mentioned. So every prompt that touches a write
        // tool must name the token, its single use and its binding.
        let mut s = server();
        for name in ["arm_a_venue", "triage_a_stuck_order"] {
            let got = s.handle(&req(1, "prompts/get", json!({ "name": name }))).unwrap();
            let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
            for needle in ["preview_token", "confirm: true", "at most ONCE", "BOUND to the command"]
            {
                assert!(text.contains(needle), "{name}'s prompt never says {needle:?}");
            }
            assert!(
                text.contains("verified_by_node"),
                "{name}'s prompt must tell the agent to read the node's verdict, not the estimate"
            );
            // ...and the ROSTER, name by name. `two_call_gate` renders it from `WRITE_TOOLS`; this
            // is what keeps it rendered. Spelled out by hand it was a FOURTH copy of the roster
            // held equal to nothing — an eighth write tool joins the routing, the `destructiveHint`
            // annotations and the transcript harness by construction, and would be missing from
            // exactly the sheet an agent reads before calling it ONCE and believing it executed.
            for tool in WRITE_TOOLS {
                assert!(text.contains(tool), "{name}'s prompt never names the write tool {tool:?}");
            }
            // The window is derived too, for the same reason one notch smaller: retuning
            // `PREVIEW_WINDOW` must not leave the teaching sheet quoting the old number.
            let window = format!("expires {} seconds", PREVIEW_WINDOW.as_secs());
            assert!(text.contains(&window), "{name}'s prompt must say {window:?}");
        }
        // The backtest flow reaches no write tool at all, and must not pretend otherwise — an
        // agent told to confirm something during a backtest looks for a gate that is not there.
        let bt =
            s.handle(&req(2, "prompts/get", json!({ "name": "backtest_a_strategy" }))).unwrap();
        let text = bt["result"]["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(!text.contains("preview_token"), "the backtest flow places no orders");
    }

    #[test]
    fn a_prompt_argument_reaches_the_rendered_text() {
        // The arguments are advertised, so they must do something — an advertised knob that
        // changes nothing is the same defect class as a settings key nothing reads.
        let mut s = server();
        let got = s
            .handle(&req(
                1,
                "prompts/get",
                json!({ "name": "triage_a_stuck_order", "arguments": { "client_order_id": "c-42" } }),
            ))
            .unwrap();
        let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("c-42"), "the coid argument must reach the text: {text}");
    }

    #[test]
    fn the_server_advertises_resources_and_lists_them() {
        let mut s = server();
        let init = s.handle(&req(1, "initialize", json!({}))).unwrap();
        assert!(
            init["result"]["capabilities"]["resources"].is_object(),
            "initialize must declare the resources capability once resources/list answers — a \
             client that does not see it never calls the method"
        );
        let listed = s.handle(&req(2, "resources/list", json!({}))).unwrap();
        let uris: Vec<&str> = listed["result"]["resources"]
            .as_array()
            .expect("resources/list returns an array")
            .iter()
            .map(|r| r["uri"].as_str().unwrap())
            .collect();
        assert!(uris.contains(&"vike://node/snapshot"), "got {uris:?}");
        assert!(uris.contains(&"vike://backtest/last"), "got {uris:?}");
        // Every advertised resource must carry the two fields a client renders it by. A URI with
        // no name is a row a human picks blind.
        for r in listed["result"]["resources"].as_array().unwrap() {
            assert!(r["name"].as_str().is_some_and(|n| !n.is_empty()), "{r} has no name");
            assert_eq!(r["mimeType"], "application/json", "{r}");
        }
    }

    #[test]
    fn an_unread_resource_is_an_error_rather_than_an_empty_document() {
        // ⚠ The distinction this pins. An agent handed `{}` for `vike://backtest/last` would
        // summarise it as a backtest that produced nothing — a different claim from "no backtest
        // has run". Same for an unknown URI: a silent empty read is a lie an agent cannot detect.
        let mut s = server();
        let miss =
            s.handle(&req(1, "resources/read", json!({ "uri": "vike://backtest/last" }))).unwrap();
        assert_eq!(miss["error"]["code"], -32002, "got {miss}");
        assert!(
            miss["error"]["message"].as_str().unwrap().contains("no backtest has run"),
            "the error must say WHICH absence it is: {miss}"
        );

        let bogus = s.handle(&req(2, "resources/read", json!({ "uri": "vike://nope" }))).unwrap();
        assert_eq!(bogus["error"]["code"], -32002, "got {bogus}");

        // The node snapshot with no `--node` is the same shape — an error naming the missing
        // configuration, never an empty snapshot an agent would read as a flat account.
        let no_node =
            s.handle(&req(3, "resources/read", json!({ "uri": "vike://node/snapshot" }))).unwrap();
        assert_eq!(no_node["error"]["code"], -32002, "got {no_node}");
        assert!(no_node["error"]["message"].as_str().unwrap().contains("--node"), "got {no_node}");
    }

    #[test]
    fn the_last_backtest_resource_serves_what_the_tool_returned() {
        // The resource is a second WAY IN to the tool's answer, not a second source of it — so
        // what it serves must be BYTE-IDENTICAL to the tool's own rendering. There is no datahub
        // here, so drive the recording seam directly rather than pretending a run happened.
        let mut s = server();
        let report = json!({ "report": { "sharpe": 1.25, "trades": 42 } });
        s.last_backtest = Some(render(&report));
        let got =
            s.handle(&req(1, "resources/read", json!({ "uri": "vike://backtest/last" }))).unwrap();
        let contents = &got["result"]["contents"][0];
        assert_eq!(contents["uri"], "vike://backtest/last");
        assert_eq!(contents["mimeType"], "application/json");
        assert_eq!(
            contents["text"].as_str().unwrap(),
            tool_ok(report.clone())["content"][0]["text"].as_str().unwrap(),
            "the resource and the tool must render the same report identically"
        );
    }

    #[test]
    fn tools_list_has_read_and_write_tools_with_correct_hints() {
        let resp = server().handle(&req(2, "tools/list", json!({}))).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"run_backtest") && names.contains(&"submit_order"));
        // The absorbed vike-mcp read surface + the closed write-verb drift are all advertised.
        for n in [
            "validate_strategy",
            "discover_params",
            "list_templates",
            "list_series",
            "run_sweep",
            "run_walk_forward",
            "modify",
            "mass_cancel",
        ] {
            assert!(names.contains(&n), "{n} must be advertised in tools/list");
        }
        for t in tools {
            let n = t["name"].as_str().unwrap();
            if WRITE_TOOLS.contains(&n) {
                assert_eq!(t["annotations"]["destructiveHint"], true, "{n} must be destructive");
                // The mandatory-preview gate is part of each write tool's advertised contract.
                let desc = t["description"].as_str().unwrap();
                assert!(desc.contains("confirm"), "{n} must document the preview gate: {desc}");
            } else {
                assert_eq!(t["annotations"]["readOnlyHint"], true, "{n} must be read-only");
            }
        }
    }

    #[test]
    fn discover_params_tool_returns_the_declared_knobs() {
        let resp = call(
            "discover_params",
            json!({ "script": "let qty = param(\"qty\", 2.5);\nfn on_bar() {}" }),
        );
        assert_eq!(resp["result"]["isError"], false);
        let params = resp["result"]["structuredContent"]["params"].as_array().unwrap();
        assert_eq!(params[0]["name"], "qty");
        assert_eq!(params[0]["default"], 2.5);
    }

    #[test]
    fn list_indicators_returns_exactly_the_host_bound_set() {
        // The tool must advertise ONLY what the Rhai host binds (vike_script::RHAI_INDICATORS),
        // never the whole `vike_indicators::registry()` — an agent acts on this list, and any
        // unbound name it is handed produces a script that silently self-disables. Both sides read
        // the SAME derived list, so this cannot pass while the tool under-reports either.
        let resp = call("list_indicators", json!({}));
        assert_eq!(resp["result"]["isError"], false);
        let inds = resp["result"]["structuredContent"]["indicators"].as_array().unwrap();
        let mut names: Vec<&str> = inds.iter().map(|i| i["name"].as_str().unwrap()).collect();
        names.sort_unstable();
        // The union, for the reason spelled out on `is_callable`: an indicator reachable only
        // through `bollinger_mid(20)` is still an indicator this tool must offer.
        let mut expected: Vec<&str> = vike_indicators::registry()
            .iter()
            .map(|m| m.name)
            .filter(|n| vike_script::is_callable(n))
            .collect();
        expected.sort_unstable();
        assert_eq!(names, expected, "list_indicators must be the Rhai host-bound set");
        assert_eq!(resp["result"]["structuredContent"]["count"], names.len());
        // ⚠ The DEFAULT response stays compact: name + category, and no per-indicator detail. The
        // roster is the whole catalog now, so a default that carried every parameter and output
        // line would spend an agent's context on indicators it never asked about.
        for i in inds {
            assert!(i["category"].as_str().is_some_and(|s| !s.is_empty()), "{i}");
            assert!(i.get("params").is_none(), "the default roster must stay compact: {i}");
        }
    }

    /// Detail is PULLED, per name or per family — the other half of the tiering above.
    #[test]
    fn list_indicators_narrows_by_name_and_by_category() {
        // Derived: whatever the host binds first, never a hard-coded name.
        let first = vike_script::RHAI_INDICATORS.first().expect("the host binds something");
        let one = call("list_indicators", json!({ "name": first }));
        assert_eq!(one["result"]["isError"], false);
        let rows = one["result"]["structuredContent"]["indicators"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], *first);
        // The two fields a compact roster CANNOT carry, and the reason detail exists at all: a
        // multi-parameter, multi-output indicator is indistinguishable from a simple one without
        // them.
        assert!(rows[0]["params"].is_array(), "a detail row must carry its parameters");
        assert!(rows[0]["outputs"].as_array().is_some_and(|o| !o.is_empty()));
        let category = rows[0]["category"].as_str().unwrap().to_string();

        let family = call("list_indicators", json!({ "category": category.to_lowercase() }));
        assert_eq!(family["result"]["isError"], false, "the category match is case-insensitive");
        let fam = family["result"]["structuredContent"]["indicators"].as_array().unwrap();
        assert!(fam.iter().any(|r| r["name"] == *first));
        assert!(fam.iter().all(|r| r["category"] == category.as_str()));

        // A name nobody can call is an ERROR, not an empty list: an empty answer reads as "that
        // family is empty in this build", which sends somebody hunting through a config.
        let miss = call("list_indicators", json!({ "name": "sma_typo" }));
        assert_eq!(miss["result"]["isError"], true);
        let bad = call("list_indicators", json!({ "category": "not-a-category" }));
        assert_eq!(bad["result"]["isError"], true);
        assert!(bad["result"]["content"][0]["text"].as_str().unwrap().contains(&category));
    }

    /// A registry indicator the host HOLDS BACK answers with the host's own REASON. An agent that
    /// reached for one — they are real indicator names, and a model has seen them — otherwise gets
    /// "unknown", re-checks its spelling, and tries again with the same name.
    ///
    /// Derived on every axis: which name is held back, and what the reason says, both come from
    /// `vike-script`. A build binding the entire registry has nothing to explain and skips — stated
    /// rather than silent, because a vacuous pass should be readable in the test, not inferred.
    #[test]
    fn list_indicators_says_why_a_registry_name_is_not_callable() {
        // ⚠ NOT `!RHAI_INDICATORS.contains(..)` any more. That set is the BARE names, and since
        // per-line accessors landed a name can be absent from it and still perfectly callable —
        // `bollinger` is. A genuinely held-back indicator is one no spelling reaches, which is
        // exactly what `is_callable` answers.
        let held = vike_indicators::registry().iter().find(|m| !vike_script::is_callable(m.name));
        let Some(held) = held else {
            return; // nothing is held back in this build
        };
        let resp = call("list_indicators", json!({ "name": held.name }));
        assert_eq!(resp["result"]["isError"], true, "a name a script cannot call is not an answer");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let why = vike_script::unbound_reason(held.name)
            .expect("a held-back registry indicator must carry a reason");
        assert!(text.contains(why), "the tool must quote the host's own reason: {text}");
    }

    /// ⚠ The `tools/list` description ships in EVERY session, whether the tool is called or not, so
    /// it must NOT enumerate the roster. It used to — `RHAI_INDICATORS.join(", ")` was spliced in,
    /// which cost 13 characters while the host bound three names and would cost kilobytes of every
    /// agent's context now that it binds the catalog.
    #[test]
    fn list_indicators_description_does_not_enumerate_the_set() {
        let resp = server().handle(&req(2, "tools/list", json!({}))).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        let tool = tools.iter().find(|t| t["name"] == "list_indicators").unwrap();
        let desc = tool["description"].as_str().unwrap();
        assert!(desc.contains("HOST-BOUND"), "{desc}");
        assert!(
            !desc.contains(&vike_script::RHAI_INDICATORS.join(", ")),
            "the description must not carry the roster: {desc}"
        );
        // A FIXED ceiling, deliberately independent of the set: the point is that this text cannot
        // grow when an indicator is added.
        const MAX_DESCRIPTION: usize = 700;
        assert!(
            desc.len() <= MAX_DESCRIPTION,
            "the tools/list description is {} bytes — it ships in every session and must stay \
             bounded; describe the tool, do not list its answer",
            desc.len()
        );
        // ...and it must tell the agent how to GET the set, or bounding it just hid the answer.
        assert!(desc.contains("category") && desc.contains("name"), "{desc}");
    }

    #[test]
    fn a_panicking_resource_read_is_an_rpc_error_not_a_dead_session() {
        // The twin of the tool pin below, for the second way in. A resource has no tool-result
        // envelope, so the caught panic surfaces as a JSON-RPC error — and the session goes on:
        // the SAME server answers the next request.
        let mut s = server();
        let resp =
            s.handle(&req(1, "resources/read", json!({ "uri": "vike://__test_panic" }))).unwrap();
        assert_eq!(resp["error"]["code"], -32603, "{resp}");
        assert!(resp["error"]["message"].as_str().unwrap().contains("panicked"), "{resp}");
        let next = s.handle(&req(2, "ping", json!({}))).unwrap();
        assert!(next.get("result").is_some(), "the session must survive a caught panic: {next}");
    }

    #[test]
    fn a_panicking_prompt_render_is_an_rpc_error_not_a_dead_session() {
        let mut s = server();
        let resp = s.handle(&req(1, "prompts/get", json!({ "name": "__test_panic" }))).unwrap();
        assert_eq!(resp["error"]["code"], -32603, "{resp}");
        assert!(resp["error"]["message"].as_str().unwrap().contains("panicked"), "{resp}");
        let next = s.handle(&req(2, "ping", json!({}))).unwrap();
        assert!(next.get("result").is_some(), "the session must survive a caught panic: {next}");
    }

    #[test]
    fn a_panicking_tool_becomes_iserror_not_a_dead_session() {
        // Mirrors vike-mcp's rpc.rs `tools_call_panic_becomes_iserror` pin: the catch_unwind in
        // `handle` turns a panicking tool into an `isError` tool RESULT — never a JSON-RPC
        // protocol error, never a crashed stdio loop.
        let resp = call("__test_panic", json!({}));
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("panicked"));
        assert!(resp.get("error").is_none(), "a caught tool panic is NOT a protocol error");
    }

    #[test]
    fn submit_order_without_confirm_only_previews_and_touches_no_network() {
        // node_addr is None; a preview must NOT try to connect (it would error). It returns the
        // resolved wire command with will_execute:false.
        let resp = call(
            "submit_order",
            json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 0.5, "order_type": "limit", "price": 100.0 }),
        );
        assert_eq!(resp["result"]["isError"], false, "a preview succeeds even with no node");
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], false);
        assert_eq!(sc["wire_command"]["Submit"]["symbol"], "BTCUSDT");
        assert_eq!(sc["wire_command"]["Submit"]["qty"], 0.5);
        assert_eq!(sc["guardrail"]["notional"], 50.0); // 0.5 * 100
    }

    /// Every WRITE tool advertises the optional `reason` (node proto v4) — and never requires it, so
    /// an agent that ignores it keeps working exactly as before.
    #[test]
    fn every_write_tool_advertises_an_optional_reason() {
        let resp = server().handle(&req(2, "tools/list", json!({}))).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        for name in WRITE_TOOLS {
            let tool = tools.iter().find(|t| t["name"] == name).expect("advertised");
            let props = &tool["inputSchema"]["properties"];
            assert_eq!(props["reason"]["type"], "string", "{name} must offer a `reason` string");
            assert!(
                props["reason"]["description"].as_str().unwrap().contains("audit"),
                "{name}: the reason description must say where it lands"
            );
            let required = tool["inputSchema"]["required"].as_array();
            assert!(
                required.is_none_or(|r| !r.iter().any(|v| v == "reason")),
                "{name}: `reason` must stay OPTIONAL"
            );
        }
        // A READ tool has no rationale to record.
        let snap = tools.iter().find(|t| t["name"] == "node_snapshot").unwrap();
        assert!(snap["inputSchema"]["properties"]["reason"].is_null());
    }

    /// The preview ECHOES the rationale (so the agent sees what will be filed against the command)
    /// while leaving the resolved command untouched — the reason rides beside it, never inside it.
    #[test]
    fn a_write_preview_echoes_the_reason_without_changing_the_command() {
        let bare = call("cancel_order", json!({ "client_order_id": "c-1" }));
        assert_eq!(bare["result"]["structuredContent"]["reason"], Value::Null);

        let withr = call(
            "cancel_order",
            json!({ "client_order_id": "c-1", "reason": "  stale quote after the feed gap  " }),
        );
        let sc = &withr["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], false);
        assert_eq!(sc["reason"], "stale quote after the feed gap", "trimmed, and echoed back");
        assert_eq!(
            sc["wire_command"], bare["result"]["structuredContent"]["wire_command"],
            "the wire command is byte-identical with and without a reason"
        );
        // A blank rationale is no rationale (never an empty string in the preview).
        let blank = call("cancel_order", json!({ "client_order_id": "c-1", "reason": "   " }));
        assert_eq!(blank["result"]["structuredContent"]["reason"], Value::Null);
    }

    #[test]
    fn submit_order_missing_required_field_is_a_tool_error() {
        let resp = call("submit_order", json!({ "venue": "sim", "side": 1, "qty": 1.0 })); // no symbol
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn confirm_alone_no_longer_executes_it_returns_a_preview() {
        // ⚠ THE REGRESSION THIS PINS. `confirm: true` used to be the whole gate, so a FIRST-and-only
        // call executed — while this module's doc promised "only a second call ... sends it". It now
        // fails CLOSED: without a `preview_token` the call is a preview, whatever `confirm` says.
        let resp = call(
            "submit_order",
            json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0, "confirm": true }),
        );
        assert_eq!(
            resp["result"]["isError"], false,
            "confirm without a token is a PREVIEW, not an error"
        );
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], false, "nothing may be sent without a preview_token");
        assert!(
            sc["preview_token"].is_string(),
            "a preview must hand back the token that confirms it"
        );
    }

    #[test]
    fn a_preview_with_no_node_says_it_was_not_verified() {
        // An ABSENT node verdict must never read as an approving one — the client-side guardrail
        // cannot price a market order at all, and market is the default order type.
        let resp = call(
            "submit_order",
            json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }),
        );
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["verified_by_node"], false);
        assert_eq!(sc["node_verdict"], Value::Null);
        assert!(
            sc["note"].as_str().unwrap().contains("UNVERIFIED CLIENT-SIDE ESTIMATE"),
            "the note must SAY the verdict is unverified: {}",
            sc["note"]
        );
    }

    #[test]
    fn a_node_that_could_not_be_asked_is_not_a_verified_preview() {
        // ⚠ THE BUG THIS PINS. `verified` used to be `node.is_some()`, and `Server::node_preview`
        // returns `Some` on its FAILURE path too — a `checked_by: "none"` row carrying the
        // transport error, so the preview can name the fault. So a node that was dialled and
        // refused (unreachable, `AuthDenied`, `Response::Error`) came back `verified_by_node: true`
        // wearing the note that calls the verdict "the one that counts" — while `node_verdict`
        // right beside it said the node was never asked. The prompts this server serves teach an
        // agent to read that flag and confirm on it; for a MARKET order the client-side guardrail
        // is vacuous too, so nothing at all would have checked the order.
        let cmd = verbs::wire_command_for(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }),
        )
        .unwrap();
        let unreachable = json!({
            "checked_by": CHECKED_BY_NONE,
            "accepted": Value::Null,
            "reason": "the node could not be asked: connection refused",
        });
        let pv = preview_of(
            "submit_order",
            &cmd,
            None,
            verbs::GuardrailCaps::default(),
            "pv-1",
            Some(unreachable),
        );
        assert_eq!(pv["verified_by_node"], false, "a fault is not a verdict: {pv}");
        assert!(
            pv["note"].as_str().unwrap().contains("UNVERIFIED CLIENT-SIDE ESTIMATE"),
            "the note must match the flag: {}",
            pv["note"]
        );
        // ...and the answering case still reads as verified, or the fix would have disarmed the
        // distinction rather than corrected it.
        let answered = json!({
            "checked_by": CHECKED_BY_NODE,
            "accepted": true,
            "reason": Value::Null,
        });
        let ok = preview_of(
            "submit_order",
            &cmd,
            None,
            verbs::GuardrailCaps::default(),
            "pv-2",
            Some(answered),
        );
        assert_eq!(ok["verified_by_node"], true, "an answered dry-run IS a verdict: {ok}");
    }

    #[test]
    fn a_preview_whose_node_could_not_be_asked_is_not_verified() {
        // A node IS configured and a control key IS present, but the dry-run cannot reach it
        // (`127.0.0.1:1` refuses immediately). `node_preview` answers with a `checked_by: "none"`
        // object rather than `None`, and `preview_of` used to label THAT `verified_by_node: true`
        // with the "it is the verdict that counts" note — a dropped tunnel mid-incident read as
        // the node's own approval. A fault is not a verdict.
        let env: std::collections::HashMap<String, String> =
            [(nodekeys::CONTROL_KEY_ENV.to_string(), "ctl-key".to_string())].into_iter().collect();
        let mut s = Server {
            datahub_addr: DEFAULT_ADDR.to_string(),
            node_addr: Some("127.0.0.1:1".to_string()),
            control: None,
            observe: None,
            caps: verbs::GuardrailCaps::default(),
            keys: nodekeys::resolve(&env, &std::collections::HashMap::new(), None),
            coids: verbs::coid_minter(),
            pending: PendingPreviews::default(),
            last_backtest: None,
        };
        let resp = s
            .handle(&req(
                1,
                "tools/call",
                json!({ "name": "cancel_order", "arguments": { "client_order_id": "c-1" } }),
            ))
            .unwrap();
        assert_eq!(resp["result"]["isError"], false, "a preview succeeds even unreachable");
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], false);
        assert_eq!(sc["node_verdict"]["checked_by"], "none", "{}", sc["node_verdict"]);
        assert_eq!(sc["node_verdict"]["accepted"], Value::Null);
        assert_eq!(sc["verified_by_node"], false, "a transport fault is not a verdict");
        assert!(
            sc["note"].as_str().unwrap().contains("UNVERIFIED CLIENT-SIDE ESTIMATE"),
            "the note must SAY the verdict is unverified: {}",
            sc["note"]
        );
    }

    #[test]
    fn a_preview_token_fires_at_most_once() {
        let mut s = server();
        let args = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 });
        let pv = s.call_tool("submit_order", &args).unwrap();
        let token = pv["preview_token"].as_str().unwrap().to_string();
        let mut confirming = args.clone();
        confirming["confirm"] = json!(true);
        confirming["preview_token"] = json!(token.clone());
        // First confirm consumes the token; with no node configured it fails at the CONNECTION,
        // which is proof it got PAST the gate.
        let first = s.call_tool("submit_order", &confirming).unwrap_err();
        assert!(first.contains("node"), "should reach the node step, got: {first}");
        // Second gets nothing — the token was removed before the caller executed.
        let second = s.call_tool("submit_order", &confirming).unwrap_err();
        assert!(
            second.contains("unknown or already used"),
            "a token must fire at most once, got: {second}"
        );
    }

    #[test]
    fn intent_ignores_the_minted_id_but_nothing_else() {
        // ⚠ THE BUG THIS PINS. An exact compare rejected EVERY confirm, because
        // `fill_client_order_id` mints a fresh id on every call — so a preview and its confirm can
        // never carry the same one, and submit_order was unusable through this surface. CI caught
        // it; this keeps it caught.
        let base = verbs::wire_command_for(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }),
        )
        .unwrap();
        let mut a = base.clone();
        let mut b = base.clone();
        if let (WireCommand::Submit(x), WireCommand::Submit(y)) = (&mut a, &mut b) {
            x.client_order_id = "c-1".into();
            y.client_order_id = "c-2".into();
        }
        assert!(same_intent(&a, &b), "a differing minted id must NOT break the binding");

        // …but every other field still binds.
        let other = verbs::wire_command_for(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 50.0 }),
        )
        .unwrap();
        assert!(!same_intent(&a, &other), "a different qty MUST break the binding");
    }

    #[test]
    fn a_preview_token_is_bound_to_the_command_it_previewed() {
        // ⚠ Preview `qty: 0.5`, confirm `qty: 50` was ACCEPTED before this gate existed.
        let mut s = server();
        let pv = s
            .call_tool(
                "submit_order",
                &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 0.5 }),
            )
            .unwrap();
        let token = pv["preview_token"].as_str().unwrap().to_string();
        let err = s
            .call_tool(
                "submit_order",
                &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 50.0,
                         "confirm": true, "preview_token": token }),
            )
            .unwrap_err();
        assert!(
            err.contains("does not match this command"),
            "a token must not confirm a DIFFERENT command, got: {err}"
        );
    }

    #[test]
    fn set_trading_state_preview_maps_the_wire_command() {
        let resp = call("set_trading_state", json!({ "state": "halted" }));
        assert_eq!(
            resp["result"]["structuredContent"]["wire_command"]["SetTradingState"],
            "Halted"
        );
    }

    #[test]
    fn market_exit_preview_allows_an_omitted_venue() {
        let resp = call("market_exit", json!({}));
        assert_eq!(resp["result"]["isError"], false);
        assert!(resp["result"]["structuredContent"]["wire_command"]["MarketExit"].is_object());
    }

    #[test]
    fn list_strategies_is_advertised_read_only() {
        let resp = server().handle(&req(3, "tools/list", json!({}))).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        let tool = tools
            .iter()
            .find(|t| t["name"] == "list_strategies")
            .expect("list_strategies must be advertised in tools/list");
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
    }

    #[test]
    fn list_strategies_without_a_reachable_datahub_is_a_clean_error() {
        // Point at a port nothing is listening on: the connect must fail into a clean tool error,
        // never a panic. (`127.0.0.1:1` refuses immediately.)
        let mut s = Server {
            datahub_addr: "127.0.0.1:1".to_string(),
            node_addr: None,
            control: None,
            observe: None,
            caps: verbs::GuardrailCaps::default(),
            keys: NodeKeyring::default(),
            coids: verbs::coid_minter(),
            pending: PendingPreviews::default(),
            last_backtest: None,
        };
        let resp = s
            .handle(&req(1, "tools/call", json!({ "name": "list_strategies", "arguments": {} })))
            .unwrap();
        assert_eq!(resp["result"]["isError"], true, "an unreachable datahub must error, not panic");
    }

    #[test]
    fn node_snapshot_without_node_is_a_clean_error() {
        let resp = call("node_snapshot", json!({}));
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn unknown_method_is_a_jsonrpc_error() {
        let resp = server().handle(&req(9, "no/such", json!({}))).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn a_notification_gets_no_response() {
        assert!(
            server()
                .handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
                .is_none()
        );
    }

    // ---- the absorbed vike-mcp tool surface (Phase A) ----------------------------------------

    #[test]
    fn validate_strategy_answers_ok_and_compile_errors_offline() {
        // A good script is an `ok:true` ANSWER; a bad script is an `ok:false` ANSWER carrying the
        // compile error — NOT an isError tool failure (the agent reads the error and fixes the
        // script). Mirrors vike-mcp's validate_strategy semantics; the argument is `script` here
        // (vike-mcp said `code`), aligned with this file's discover_params.
        let good = call(
            "validate_strategy",
            json!({ "script": "let fast = param(\"fast\", 5.0);\nfn on_bar() {}" }),
        );
        assert_eq!(good["result"]["isError"], false);
        assert_eq!(good["result"]["structuredContent"]["ok"], true);

        let bad = call("validate_strategy", json!({ "script": "fn on_bar( {" }));
        assert_eq!(bad["result"]["isError"], false, "a compile error is an ANSWER, not a failure");
        let sc = &bad["result"]["structuredContent"];
        assert_eq!(sc["ok"], false);
        assert!(!sc["error"].as_str().unwrap().is_empty());
    }

    #[test]
    fn validate_strategy_missing_script_arg_is_a_tool_error() {
        let resp = call("validate_strategy", json!({}));
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("script"));
    }

    #[test]
    fn list_templates_returns_named_parameterized_sources_that_compile() {
        let resp = call("list_templates", json!({}));
        assert_eq!(resp["result"]["isError"], false);
        let ts = resp["result"]["structuredContent"]["templates"].as_array().unwrap();
        assert!(ts.iter().any(|t| t["name"] == "SMA cross"));
        // Every advertised template must actually compile against THIS build's Rhai host and
        // expose param() knobs (so it drops straight into run_sweep) — the same pin
        // vike-studio-core keeps on its own copy of these sources.
        //
        // ⚠ Compiling is NOT the property that matters most, and this test cannot see it:
        // `discover_params` runs the script's TOP LEVEL only, and every template's real work sits
        // inside `fn on_bar()`. `every_shipped_template_reaches_the_broker` below is the gate that
        // covers what this one structurally cannot.
        for t in ts {
            let (name, code) = (t["name"].as_str().unwrap(), t["code"].as_str().unwrap());
            let params = vike_script::discover_params(code)
                .unwrap_or_else(|e| panic!("template {name} must compile: {e}"));
            assert!(!params.is_empty(), "template {name} must expose param()s");
            assert!(code.contains("on_bar"), "template {name} must define on_bar");
        }
    }

    /// A deterministic strictly-RISING bar series. Monotone closes are enough to drive every
    /// shipped template past its warm-up and into a decision: `sma(5)` rises above `sma(20)`,
    /// `rsi(14)` climbs past the reversion band's `hi`, and `high()` clears the breakout channel.
    fn rising_bars(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let c = 100.0 + i as f64;
                Bar {
                    ts: i as i64 * 60_000,
                    open: c,
                    high: c,
                    low: c,
                    close: c,
                    volume: 1.0,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: Some("BTCUSDT".into()),
                }
            })
            .collect()
    }

    /// ⚠ **The gate behind "an agent can copy a template and it will actually trade."**
    ///
    /// `list_templates` is the surface an AGENT copies from, and until this test it was the one
    /// template surface with no behavioral gate at all: the compile pin above is the whole of what
    /// this crate checked. (`crates/vike-studio-core/tests/templates_execute.rs` is the twin over
    /// that crate's own copy of these sources; it runs them through the real Run pipeline instead.)
    ///
    /// Parsing is the wrong bar. Rhai resolves a REGISTERED function when its line RUNS, not at
    /// compile time, and `discover_params` runs only the top level — so every call a template makes
    /// (all of them inside `fn on_bar()`) is unchecked by a compile gate. Three mistakes land in
    /// that blind spot identically: an unbound NAME (a typo, or any name outside
    /// `vike_script::RHAI_INDICATORS` — the set the host actually registers), a wrong
    /// ARITY, and a wrong ARGUMENT TYPE (`rhai`'s `resolve_fn` hashes each argument's `TypeId` and
    /// performs no INT->FLOAT coercion for a registered function, so `market(1, 1)` or `sma(5.0)`
    /// misses just as hard as a typo). Each raises `ErrorFunctionNotFound` on every bar;
    /// `RhaiStrategy`'s hook runner swallows it (fail-safe: zero orders that bar) and self-disables
    /// after 10 consecutive errors. The strategy looks mounted and silently never trades. Only
    /// EXECUTION distinguishes that from a strategy that simply saw no signal.
    ///
    /// Non-vacuity is demonstrated rather than argued — see
    /// `an_unbound_call_compiles_and_then_reaches_the_broker_with_nothing` below, which drives the
    /// same series and shows this assertion genuinely fails for such a script.
    #[test]
    fn every_shipped_template_reaches_the_broker() {
        for (name, src) in TEMPLATES {
            let mut strat = RhaiStrategy::<MockBroker>::compile(src)
                .unwrap_or_else(|e| panic!("template {name} must compile: {e}"));
            let mut broker = MockBroker::default();
            for bar in rising_bars(80) {
                broker.px = bar.close;
                strat.on_bar(&mut broker, &bar);
            }
            assert!(
                !broker.markets.is_empty(),
                "template {name} placed no order over 80 rising bars — a template an agent COPIES \
                 must actually trade. Check that every function it calls is host-bound \
                 (`vike_script::RHAI_INDICATORS`, plus `crates/vike-script/src/engine.rs`'s \
                 `register_reads`/`register_verbs`/`build_engine`) and that each call's arity and \
                 ARGUMENT TYPES match the registration exactly — rhai coerces neither."
            );
            for (symbol, side, qty) in &broker.markets {
                assert_eq!(symbol, "BTCUSDT", "template {name} must route to the bar's own symbol");
                assert!(*side == 1 || *side == -1, "template {name} sent side {side}, not ±1");
                assert!(*qty > 0.0, "template {name} sent a non-positive qty {qty}");
            }
        }
    }

    /// The proof that the gate above is not vacuous, and a live specimen of the failure it exists
    /// to catch.
    ///
    /// The SMA-cross template with its `sma(` calls rewritten to [`UNBOUND_WITNESS`] still COMPILES
    /// and still reports its `param()` knobs — both compile-only gates stay green on it — while
    /// reaching the broker exactly never.
    ///
    /// ⚠ The witness used to be `wma`, a REAL registry indicator the host did not bind. It stopped
    /// being a witness when the host widened to the registry, so it is now a name outside the
    /// registry ENTIRELY — asserted, not assumed, on both axes below. Do not restore `wma`: a
    /// bound name here makes this test pass for the wrong reason and quietly turns
    /// `every_shipped_template_reaches_the_broker` into a claim about nothing.
    #[test]
    fn an_unbound_call_compiles_and_then_reaches_the_broker_with_nothing() {
        let unbound = SMA_CROSS.replace("sma(", &format!("{UNBOUND_WITNESS}("));
        assert!(
            unbound.contains(&format!("{UNBOUND_WITNESS}(")),
            "test premise: the rewrite must have applied"
        );
        assert!(
            vike_script::discover_params(&unbound).is_ok(),
            "test premise: a call to an unbound function still COMPILES — that is the whole hazard"
        );
        assert!(
            !vike_script::RHAI_INDICATORS.contains(&UNBOUND_WITNESS),
            "test premise: the witness must stay outside the host-bound set"
        );
        assert!(
            vike_indicators::get(UNBOUND_WITNESS).is_none(),
            "test premise: the witness must not be a registry indicator either"
        );

        let mut strat = RhaiStrategy::<MockBroker>::compile(&unbound).expect("still compiles");
        let mut broker = MockBroker::default();
        for bar in rising_bars(80) {
            broker.px = bar.close;
            strat.on_bar(&mut broker, &bar);
        }
        assert!(
            broker.markets.is_empty(),
            "a script calling an unbound host function must reach the broker with NOTHING — \
             otherwise `every_shipped_template_reaches_the_broker` could not detect one"
        );
    }

    /// A minimal, VALID sweep profile (shape errors must never be what a connect test trips on).
    const SWEEP_PROFILE: &str = "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\nfrom = \"0\"\nto = \"100000\"\n[strategy]\nname = \"buy_hold\"\n[sweep]\nfast = [5, 10]\n";

    /// The tool's OWN argument checks still fire before any connect. Profile-SHAPE errors (no
    /// `[sweep]`/`[walkforward]` table, bad range, unknown strategy) deliberately moved SERVER-side
    /// when these tools started shipping the TOML verbatim — one profile parser in the workspace,
    /// one error source; the server's `run_sweep_profile` arm answers them (pinned by
    /// `vike-datahub`'s `run_sweep_walkforward_profile_roundtrip` test).
    #[test]
    fn run_tools_reject_a_missing_profile_arg_before_any_connect() {
        for tool in ["run_sweep", "run_walk_forward"] {
            let resp = call(tool, json!({}));
            assert_eq!(resp["result"]["isError"], true, "{tool}");
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("profile"), "{tool}: {text}");
        }
        // An unparsable `script` injection is likewise a local error (it rewrites the TOML here).
        let resp = call("run_sweep", json!({ "profile": "this is [not valid", "script": "x" }));
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn remote_run_tools_without_a_reachable_datahub_are_clean_errors() {
        // Point at a port nothing is listening on (`127.0.0.1:1` refuses immediately): each remote
        // tool's connect failure must be a clean tool error, never a panic — same pin as
        // `list_strategies_without_a_reachable_datahub_is_a_clean_error`.
        let mut s = Server {
            datahub_addr: "127.0.0.1:1".to_string(),
            node_addr: None,
            control: None,
            observe: None,
            caps: verbs::GuardrailCaps::default(),
            keys: NodeKeyring::default(),
            coids: verbs::coid_minter(),
            pending: PendingPreviews::default(),
            last_backtest: None,
        };
        let wf_profile = format!("{SWEEP_PROFILE}[walkforward]\nn_splits = 4\n");
        for (tool, args) in [
            ("run_sweep", json!({ "profile": SWEEP_PROFILE })),
            ("run_walk_forward", json!({ "profile": wf_profile })),
            ("list_series", json!({})),
        ] {
            let resp = s
                .handle(&req(1, "tools/call", json!({ "name": tool, "arguments": args })))
                .unwrap();
            assert_eq!(resp["result"]["isError"], true, "{tool} must error, not panic");
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("cannot connect"), "{tool}: {text}");
        }
    }

    #[test]
    fn modify_without_confirm_only_previews_the_shared_wire_command() {
        // The verbs-module construction site: the SAME WireCommand::Modify the trade REPL's
        // `modify c-1 --qty 2` builds, preview-gated like every other write tool.
        let resp = call("modify", json!({ "client_order_id": "c-1", "new_qty": 2.0 }));
        assert_eq!(resp["result"]["isError"], false, "a preview succeeds even with no node");
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], false);
        assert_eq!(sc["wire_command"]["Modify"]["client_order_id"], "c-1");
        assert_eq!(sc["wire_command"]["Modify"]["new_qty"], 2.0);
        assert_eq!(sc["wire_command"]["Modify"]["new_price"], Value::Null);
        assert_eq!(sc["guardrail"]["within_limits"], true, "no order size to check");
    }

    #[test]
    fn modify_with_nothing_to_change_is_a_tool_error() {
        let resp = call("modify", json!({ "client_order_id": "c-1" }));
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("new_qty"));
    }

    #[test]
    fn mass_cancel_preview_allows_an_omitted_scope() {
        let resp = call("mass_cancel", json!({}));
        assert_eq!(resp["result"]["isError"], false);
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], false);
        assert_eq!(sc["wire_command"]["MassCancel"]["venue"], Value::Null);
        assert_eq!(sc["wire_command"]["MassCancel"]["symbol"], Value::Null);
    }

    #[test]
    fn mass_cancel_confirm_without_a_token_is_a_preview_not_a_send() {
        let resp = call("mass_cancel", json!({ "venue": "sim", "confirm": true }));
        assert_eq!(resp["result"]["isError"], false, "no token ⇒ preview, never a send");
        assert_eq!(resp["result"]["structuredContent"]["will_execute"], false);
    }

    /// Every `skills/<name>/SKILL.md` teaches a procedure over THIS server's tools, and nothing but
    /// the file itself says which tools those are — so this is the one place a tool change can
    /// redden the skill that teaches it. The failure it guards is the M1 one: the docs described
    /// the preview gate for a full day after the gate had changed and nobody noticed, because a
    /// page that names a tool is never compiled against the tool. A skill is worse than a page
    /// here — it is INSTALLED (`npx skills add` symlinks the file into an agent's skill
    /// directory), so a stale step is not read by a human who might doubt it; it is executed by an
    /// agent that will not.
    ///
    /// What is pinned, per skill, against the Agent Skills spec and against `tools_spec`:
    ///   * `name` equals the directory (the spec's identity rule) and matches its pattern;
    ///   * `description` is 30–1024 chars — it is the TRIGGER (the spec has no separate field),
    ///     so an empty one is a skill that never fires and an overlong one is refused at install;
    ///   * every read field is a scalar a YAML parser would accept — no `: ` or ` #` inside an
    ///     unquoted value, no leading indicator — because this scan is not a parser and once
    ///     passed a description js-yaml refused (the file is the product, and it did not install);
    ///   * under 500 lines, the spec's ceiling;
    ///   * every `metadata.tools` entry is a tool this server serves — a renamed tool reddens here;
    ///   * every served tool the body names in backticks IS declared, so `metadata.tools` cannot
    ///     under-report what the skill drives (the convention this buys: backticks mean "a tool
    ///     this procedure calls", and a tool merely referred to is written bare);
    ///   * a declared WRITE tool comes with `preview_token` in the body — a skill that teaches a
    ///     write without the two-call gate teaches an agent to read a preview as a send;
    ///   * the text names no operator file the public mirror withholds (`CLAUDE.md`, `justfile`,
    ///     `scripts/`, `.github/`, the decision and plan trees) and pins no `.rs:NNN` line — both
    ///     rot silently, and neither can be followed from an install.
    ///
    /// Read from disk at test time, deliberately not `include_str!`: the mirror ships `skills/`
    /// beside `crates/`, and a compile-time embed would make ten markdown files build inputs of
    /// this crate. The count floor keeps the test from passing over an empty or mis-resolved
    /// directory.
    #[test]
    fn every_skill_names_only_tools_this_server_serves() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
        let served: Vec<String> = tools_spec()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("skills directory {}: {e}", dir.display()))
            .flatten()
            .collect();
        entries.sort_by_key(|e| e.file_name());
        let mut seen = 0;
        for entry in entries {
            let path = entry.path().join("SKILL.md");
            if !path.is_file() {
                continue;
            }
            let skill = entry.file_name().to_string_lossy().to_string();
            let text = std::fs::read_to_string(&path).unwrap();
            seen += 1;
            // Frontmatter is the block between the opening `---` and the next; the body follows.
            let rest =
                text.strip_prefix("---\n").unwrap_or_else(|| panic!("{skill}: no frontmatter"));
            let end =
                rest.find("\n---\n").unwrap_or_else(|| panic!("{skill}: unterminated frontmatter"));
            let (front, body) = (&rest[..end], &rest[end + 5..]);
            // A top-level or `metadata:`-nested `key: value` line, as written. No YAML
            // dependency: the two shapes the spec allows here are both one line.
            let raw_field = |key: &str| -> Option<&str> {
                front
                    .lines()
                    .find_map(|l| l.trim_start().strip_prefix(key)?.strip_prefix(':'))
                    .map(str::trim)
            };
            // ...and the same value with its quotes stripped, for the content assertions below.
            let field = |key: &str| raw_field(key).map(|v| v.trim_matches('"'));
            // ⚠ A line scan is not a YAML parser, and the gap bit once: a description quoting the
            // error text `control command not sent: Gone` passed here — it is 30–1024 characters —
            // while js-yaml (what `npx skills add` and gray-matter wrap) refused the file with
            // `bad indentation of a mapping entry`, because `: ` inside an UNQUOTED scalar ends
            // the scalar. So every field the scan reads is held to the plain-scalar rules a YAML
            // parser would apply, unless the whole value is one quoted scalar: no `: ` or ` #`
            // inside it, no trailing `:`, and no leading indicator character. Read from disk by a
            // parser this crate does not carry, the failure was an uninstallable skill behind a
            // green gate; here it is a named substring.
            let plain_scalar_hazard = |raw: &str| -> Option<String> {
                let quoted = |q: char| raw.len() >= 2 && raw.starts_with(q) && raw.ends_with(q);
                if raw.is_empty() || quoted('"') || quoted('\'') {
                    return None;
                }
                for needle in [": ", " #"] {
                    if raw.contains(needle) {
                        return Some(format!("contains {needle:?}"));
                    }
                }
                if raw.ends_with(':') {
                    return Some("ends with ':'".to_string());
                }
                let first = raw.chars().next().unwrap_or(' ');
                if "-?:,[]{}#&*!|>'\"%@`".contains(first) {
                    return Some(format!("starts with the indicator {first:?}"));
                }
                None
            };
            for key in ["name", "description", "tools", "source"] {
                if let Some(raw) = raw_field(key)
                    && let Some(why) = plain_scalar_hazard(raw)
                {
                    panic!(
                        "{skill}: frontmatter `{key}` is an unquoted scalar that {why} — a YAML \
                         parser ends the value there and refuses the file, so the skill cannot \
                         be installed. Reword it, or quote the whole value"
                    );
                }
            }
            let name = field("name").unwrap_or_else(|| panic!("{skill}: no `name`"));
            assert_eq!(name, skill, "{skill}: `name` must equal the directory name");
            let pattern_ok = !name.is_empty()
                && name.len() <= 64
                && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !name.starts_with('-')
                && !name.ends_with('-')
                && !name.contains("--");
            assert!(pattern_ok, "{skill}: `name` {name:?} breaks the spec's pattern");
            let desc_chars = field("description").unwrap_or_default().chars().count();
            assert!(
                (30..=1024).contains(&desc_chars),
                "{skill}: description is {desc_chars} chars, must be 30–1024 — it is the trigger"
            );
            let lines = text.lines().count();
            assert!(lines < 500, "{skill}: {lines} lines; the spec's ceiling is 500");
            let declared: Vec<&str> =
                field("tools").unwrap_or_default().split_whitespace().collect();
            for t in &declared {
                assert!(
                    served.iter().any(|s| s == *t),
                    "{skill}: metadata.tools names `{t}`, which this server does not serve \
                     (tools_spec serves {served:?})"
                );
            }
            for s in &served {
                if body.contains(&format!("`{s}`")) {
                    assert!(
                        declared.contains(&s.as_str()),
                        "{skill}: the body names `{s}` but metadata.tools does not declare it — \
                         the declaration under-reports what the skill drives"
                    );
                }
            }
            for t in &declared {
                if WRITE_TOOLS.contains(t) {
                    assert!(
                        body.contains("preview_token"),
                        "{skill}: teaches the write tool `{t}` without `preview_token` — without \
                         the two-call gate every call is a preview, and an agent taught to read \
                         one as a send has been taught a refusal"
                    );
                }
            }
            for needle in [
                "CLAUDE.md",
                "justfile",
                "scripts/",
                ".github/",
                "docs/decisions",
                "docs/superpowers",
            ] {
                assert!(
                    !text.contains(needle),
                    "{skill}: names {needle}, which the mirror withholds"
                );
            }
            for (i, line) in text.lines().enumerate() {
                let mut tail = line;
                while let Some(at) = tail.find(".rs:") {
                    tail = &tail[at + 4..];
                    assert!(
                        !tail.starts_with(|c: char| c.is_ascii_digit()),
                        "{skill} line {}: {:?} pins a line number — cite by symbol",
                        i + 1,
                        line.trim()
                    );
                }
            }
        }
        assert!(
            seen >= 10,
            "only {seen} skills under {} — the package ships ten, so a smaller count is a wrong \
             path, not a smaller package",
            dir.display()
        );
    }
}
