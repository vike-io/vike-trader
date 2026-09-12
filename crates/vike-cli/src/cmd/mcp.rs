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
//! `list_series` (remote datahub metadata), `node_snapshot`, `strategy_status`, `settings_show`
//! (the two PER-CALL node reads — see [`Server::tool_strategy_status`] for why they open their own
//! short-lived connection rather than riding the held observe pipe). This is the ABSORBED `vike-mcp` tool
//! surface (Phase A of retiring that crate): the run/list tools go over the datahub RPC verbs
//! instead of a local DataFusion store, so this CLI stays DataFusion-free; `validate_strategy` /
//! `list_templates` are offline ports (note the argument is named `script` here — vike-mcp said
//! `code` — matching this file's existing `discover_params`).
//!
//! LIVE WRITE (`destructiveHint`), gated — two families on ONE roster ([`WRITE_TOOLS`]) because
//! what the roster BUYS is the same for both: the mandatory preview gate, the `destructiveHint`,
//! the `read-only` withholding and the transcript's write classification.
//!
//!   * the ORDER verbs — `submit_order`, `cancel_order`, `modify`, `flatten`, `market_exit`,
//!     `set_trading_state`, `mass_cancel`;
//!   * the NODE-LIFECYCLE verbs ([`LIFECYCLE_TOOLS`]) — `mount_strategy`, `unmount_strategy`,
//!     `set_setting` — which change what the node RUNS and how it is CONFIGURED rather than what
//!     is in its book.
//!
//! **Both families are built by the SAME [`crate::cmd::verbs`] construction site the `trade` REPL
//! resolves through** (one vocabulary, two surfaces), and this server keeps only its JSON argument
//! SHAPE. The lifecycle three were built HERE for as long as they had no REPL spelling; they got
//! one, so they moved — `verbs`' module doc carries that history. `set_setting` on `policy.toml`
//! additionally carries a TYPED confirm that this server may never fill in, and whose GATE stays
//! here rather than in the shared site because the two surfaces answer it differently on purpose —
//! [`policy_confirm_property`] and [`typed_confirm_verdict`].
//!
//! **Mandatory preview, and it is now actually mandatory:** a
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
//! environment, else from the node-key store the daemon itself reads
//! (`<project>/settings/node.env`, see [`crate::cmd::nodekeys`]); absent from BOTH, the trade
//! tools return a clean error naming both places and the create+backtest tools still work.
//!
//! **A venue the node does not mount is REFUSED at the preview, before a token is minted.** The
//! gate is [`Server::vet_commanded_venue`], called from the router for every write tool, so an
//! eighth one inherits it; that function carries the mechanism it closes, read off the write path,
//! and `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md` carries the
//! disposition. The short version, because it is not what the layering suggests: **nothing between
//! the tool argument and the venue edge ever compared the venue string to anything.** The node's
//! own dry-run vets the notional cap and nothing else, and `vike_core`'s
//! `CoreThread::apply_intent_routed` resolves an unroutable venue with `unwrap_or(0)` — so it lands
//! on the node's FIRST engine with `preflight_order_at`'s unknown-venue affordance skipping every
//! capability check. A real order really rested in a paper book under `venue: "node"`, a string
//! `vike_model::VENUES` does not contain, in the 2026-09-06 model run (`submit-a-limit-order`); the
//! only check that failed was the one saying the agent had never called `node_snapshot`.
//! Two properties worth carrying before reading that function: the comparison is against the NODE's
//! reported mounted set rather than the shipped roster (a paper engine may legitimately sit behind
//! a non-roster id), and it is EXACT rather than case-folded (routing compares with `==`, so a
//! looser refusal admits strings the routing then silently redirects). Which of the three answers
//! the gate gave is reported as `venue_check` on the preview AND on the answer to the confirmed
//! call — see [`VENUE_CHECK_MOUNTED`]. The end-to-end proof against a REAL node lives in
//! `crates/vike-cli/src/cmd/mcp_node_drop_tests.rs`'s
//! `the_venue_gate_reads_a_real_nodes_mounted_set_and_refuses_what_is_not_in_it`, and it is the
//! half that holds the EVIDENCE — [`Server::mounted_venues`]'s projection of the node's own frame,
//! whose failure mode is a silent fail-OPEN — rather than the decision.
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
//! # Scoping: `--profile`, and why the roster and the router are ONE derivation
//!
//! `--profile <full|read-only|offline>` (default `full`, so an absent flag is byte-identical to the
//! surface that shipped before it existed) decides which tools this server SERVES. Under
//! `read-only` every [`WRITE_TOOLS`] member is absent from `tools/list` AND refused by
//! [`Server::call_tool`] with a message naming the profile; under `offline` every network-touching
//! tool goes too. `--deny-tool NAME` (repeatable) subtracts from whatever the profile allows — one
//! mechanism, not a second one: it feeds the SAME set the profile computes.
//!
//! ⚠ **There is no second list anywhere, and that is the property to preserve.**
//! [`ToolAccess::new`] makes ONE pass over [`tools_spec`] and produces two complements
//! (`allowed` / `withheld`); `tools/list` renders the first and [`Server::call_tool`] refuses the
//! second. Each ring is a PREDICATE over data that already existed rather than a roster somebody
//! typed:
//!
//!   * `read-only` is `!`[`is_write_tool`] — the same roster the mandatory-preview gate routes on
//!     and the `destructiveHint` annotations are already pinned equal to, so a NEW write tool
//!     joins this ring the moment it joins that array — which is exactly what the three
//!     node-lifecycle verbs did, with no edit to this ring at all;
//!   * `offline` is `annotations.openWorldHint == false` — the tool's OWN declaration that it
//!     touches no server, read back off the spec it advertises. A tool that declares NOTHING is
//!     withheld rather than admitted: the safe direction for a ring whose whole promise is "this
//!     process opens no socket".
//!
//! An UNKNOWN profile name is a usage error naming the valid ones and never widens to `full` — the
//! failure the rival surface this was measured against gets right and which a `_ => Full` fallback
//! arm would get catastrophically wrong. `the_roster_and_the_router_are_one_derivation` is the pin.
//!
//! ⚠ **The RESOURCES are scoped by the same predicate**, because a resource is "a second way in to
//! a tool's implementation, never a second source" — [`Server::read_resource`] routes
//! `vike://node/snapshot` straight into [`Server::tool_node_snapshot`], so an unscoped resource list
//! would hand back a withheld tool's answer through a URI. [`RESOURCE_TOOLS`] names the tool each
//! resource reaches and is held equal to [`resources_spec`] by
//! `every_resource_is_a_way_in_to_a_named_tool`. Neither resource is a WRITE (and none ever will
//! be), so `read-only` withholds nothing here; `offline` withholds both.
//!
//! The PROMPTS are not filtered — a prompt executes nothing, and a sheet that VANISHES teaches an
//! agent less than one that says the tools are not served here. [`two_call_gate`] therefore derives
//! its roster from the ADMITTED write set and, when that is empty, renders the absence instead of
//! instructions for tools this session does not have.
//!
//! # `instructions` — teaching the surface what lies OUTSIDE it
//!
//! `initialize` carries an `instructions` string, and it is the only channel in this protocol that
//! can say what a roster of tools cannot: **this server is one surface of a larger product, and
//! here is what the operator runs for the rest.** [`instructions`] is the text and argues its own
//! length; what belongs here is why a per-tool description could never have carried it. A tool
//! description describes ITS tool. "There is no tool for this, and the command that does it is
//! `vike-backend datahub --record`" is a fact about the ABSENCE of a tool, so there is no
//! description it could be
//! written in — and an agent that cannot state it can only refuse blind, which is what the
//! 2026-09-06 model run measured four times over.
//!
//! ⚠ **It is SCOPED by [`ToolAccess`], for the same reason `tools/list` is.** Text that tells a
//! `read-only` agent about a preview gate, or an `offline` one to confirm a fetch with
//! `list_series`, is advertising a withheld tool in prose.
//!
//! ⚠ **It names READ credential verbs and no writer, and that is a decision rather than an
//! oversight** — `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`
//! and `the_mcp_surface_advertises_no_credential_writer`, which now gates this text as well as the
//! tool roster.
//!
//! # The agent transcript: `--trace`
//!
//! `vike-tradehub` records one audit line per ACCEPTED control command. Nothing recorded what the
//! AGENT attempted — the tools, the arguments, and above all what was REFUSED and why, which never
//! reaches the node's trail at all because it was refused before a byte left this process.
//! `--trace` (or `--trace-dir <dir>`) turns on [`crate::cmd::mcp_trace`]: one appended JSONL record
//! per `tools/call`, arguments redacted through the workspace's one credential-name authority.
//!
//! ⚠ **OFF unless asked for**, and the disposition is argued in
//! `docs/decisions/0039-the-agent-transcript-is-opt-in-and-argument-redacted.md` rather than here.
//! The half that belongs in this file: the classification is derived in [`Server::handle`] from the
//! same value the response is built from, so the record cannot disagree with what the client was
//! told, and a trace failure is an `eprintln!` — **never** a `println!`, because this server's
//! STDOUT is the protocol.
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
use std::path::{Path, PathBuf};
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
use crate::cmd::mcp_trace::{self, McpTrace, TOKEN_MINTED, TOKEN_PRESENTED, Verdict};
use crate::cmd::nodekeys::{self, NodeKeyring};
use crate::cmd::verbs;

/// The datahub address the `run_backtest` tool ships profiles to (mirrors the `backtest` command's
/// default; overridable with `--addr`).
const DEFAULT_ADDR: &str = "127.0.0.1:7878";
/// The COMPUTE daemon's address the `run_backtest`/`run_sweep`/`run_walk_forward`/`list_strategies`
/// tools ship to (overridable with `--backtest-addr`). Through `vike_config` so the client and the
/// daemon cannot answer differently — see that constant for the port-collision warning it carries.
const DEFAULT_BACKTEST_ADDR: &str = vike_config::DEFAULT_BACKTEST_ADDR;
const SERVER_NAME: &str = "vike-cli";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The MCP protocol version we default to when a client does not declare one (we ECHO the client's
/// requested version on `initialize` when present). `2024-11-05` is the widely-supported baseline.
const DEFAULT_PROTOCOL: &str = "2024-11-05";
const USAGE: &str = "usage: vike-cli mcp [--addr 127.0.0.1:7878] [--backtest-addr 127.0.0.1:7880] \
                     [--node <host:port>] \
                     [--profile full|read-only|offline] [--deny-tool NAME]... \
                     [--trace | --trace-dir <dir>]";
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
    /// The COMPUTE daemon's address — where the four `Run*`/`list_strategies` tools go since
    /// ruling 7 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` moved
    /// those verbs off the data server. A SECOND field rather than a second meaning for
    /// `datahub_addr`, because this server holds tools on BOTH planes at once: `list_series` still
    /// dials the datahub and `run_backtest` no longer can.
    backtest_addr: String,
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
    /// WHICH tools this server serves, resolved ONCE at startup from `--profile` and `--deny-tool`.
    /// The advertised roster and the routing gate are both read off this one value — see
    /// [`ToolAccess`].
    access: ToolAccess,
    /// The agent transcript, when `--trace`/`--trace-dir` asked for one. `None` is the default and
    /// means NOTHING is written and no directory is created — see [`crate::cmd::mcp_trace`].
    trace: Option<McpTrace>,
    /// The DATAHUB node keys, for the one tool that needs an authenticated connection.
    ///
    /// ⚠ **This was a DECLARED GAP until the dispatcher started filling it, and the shape of the
    /// fix is the point.** `delete_series` is served only by a KEYED datahub
    /// (`vike_datahub_client::proto`'s `FEATURE_DELETE_SERIES` carries why), so a `None` here means
    /// the plain unauthenticated `DatahubClient::connect` — which a server requiring auth refuses.
    ///
    /// It could not be closed inside `src/cmd/`: an `env::var` here would be a new `Layer::Library`
    /// row on `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` ratchet, and a
    /// credential-store read a new `CREDENTIAL_STORE_PIN` entry — both ratchets that may shrink and
    /// never grow. So the keys arrive as a PARAMETER from the dispatcher, exactly as
    /// [`NodeKeyring`] already does for the TRADEHUB pair: one call to
    /// `vike_datahub_client::node_auth::node_keys_from_vars` over the sweep `crate::run` already
    /// owns, threaded through [`run`]. `crate::Resolved::datahub_keys` is the field that carries it.
    ///
    /// `None` remains the ordinary answer on a box that set neither key, and it still reaches the
    /// unauthenticated constructor — so a key-less datahub behaves exactly as it did before. What
    /// changed is only that a KEYED one is now reachable at all.
    datahub_keys: Option<vike_datahub_client::NodeKeys>,
}

/// The scoping RINGS. `full` ⊃ `read-only` ⊃ `offline`, and each inner ring is a PREDICATE over
/// data this file already had rather than a roster somebody typed — the module doc argues both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Profile {
    /// Every tool this server implements. The default, so an absent `--profile` is byte-identical
    /// to the surface that shipped before the flag existed.
    Full,
    /// Everything except the mandatory-preview write tools — `!`[`is_write_tool`], so the ring
    /// cannot drift from the roster the gate itself routes on.
    ReadOnly,
    /// Only the tools that declare they open no socket (`annotations.openWorldHint == false`): the
    /// offline authoring set. A tool declaring nothing is WITHHELD, because the promise of this
    /// ring is negative and a missing declaration is not evidence for it.
    Offline,
}

/// The spelling of each ring, and the ONLY place a profile name is parsed or printed.
///
/// An array rather than a `match` in three places: the parse, the usage message and the refusal all
/// read it, so an added ring cannot reach one of them and miss another.
const PROFILES: [(&str, Profile); 3] =
    [("full", Profile::Full), ("read-only", Profile::ReadOnly), ("offline", Profile::Offline)];

impl Profile {
    /// Parse a `--profile` value.
    ///
    /// ⚠ **An unknown name is an ERROR and never a fallback.** A `_ => Full` arm would mean a typo
    /// in an MCP client's launch config silently serves the order-write tools to an agent the
    /// operator believed was scoped down — the exact failure a scoping feature exists to prevent.
    fn parse(name: &str) -> Result<Self, String> {
        PROFILES.iter().find(|(n, _)| *n == name).map(|(_, p)| *p).ok_or_else(|| {
            format!("unknown --profile {name:?} — valid profiles: {}", profile_names().join(", "))
        })
    }

    /// The wire/CLI spelling.
    fn as_str(self) -> &'static str {
        PROFILES.iter().find(|(_, p)| *p == self).map(|(n, _)| *n).unwrap_or("full")
    }

    /// Does this ring admit the tool this [`tools_spec`] entry describes?
    fn admits_spec(self, tool: &Value) -> bool {
        match self {
            Profile::Full => true,
            Profile::ReadOnly => !is_write_tool(tool["name"].as_str().unwrap_or_default()),
            Profile::Offline => tool["annotations"]["openWorldHint"] == json!(false),
        }
    }
}

/// Every profile name, in declaration order — for the parse error and the usage text, which must
/// name the same set the parser accepts.
fn profile_names() -> Vec<&'static str> {
    PROFILES.iter().map(|(n, _)| *n).collect()
}

/// WHICH tools this server serves — the one value `tools/list`, `tools/call`, `resources/list` and
/// `resources/read` all consult.
///
/// ⚠ **`allowed` and `withheld` are complements produced by ONE pass** over [`tools_spec`]. That is
/// the whole design: an advertised roster and a routing gate computed separately are two lists that
/// can disagree, and a tool that is advertised but refused — or worse, withheld but routed — is
/// precisely the bug a scoping feature must not ship with.
#[derive(Debug, Clone)]
pub(crate) struct ToolAccess {
    profile: Profile,
    /// The `--deny-tool` names, each already validated against the served roster.
    denied: Vec<String>,
    /// Served ∧ admitted.
    allowed: Vec<String>,
    /// Served ∧ NOT admitted — the complement, from the same pass.
    withheld: Vec<String>,
}

impl ToolAccess {
    /// Resolve a profile plus a deny list into the served/withheld split.
    ///
    /// A `denied` name this server does not serve is the CALLER's problem to reject (see
    /// [`parse_config`]): a silently-ignored `--deny-tool` is an operator believing a tool is gone.
    pub(crate) fn new(profile: Profile, denied: Vec<String>) -> Self {
        let spec = tools_spec();
        let (mut allowed, mut withheld) = (Vec::new(), Vec::new());
        for tool in spec.as_array().map(Vec::as_slice).unwrap_or_default() {
            let name = tool["name"].as_str().unwrap_or_default().to_string();
            if profile.admits_spec(tool) && !denied.contains(&name) {
                allowed.push(name);
            } else {
                withheld.push(name);
            }
        }
        Self { profile, denied, allowed, withheld }
    }

    /// The unrestricted access — the default, and what every test that predates the flag means by
    /// "a server".
    pub(crate) fn full() -> Self {
        Self::new(Profile::Full, Vec::new())
    }

    /// The profile's name, for a refusal message and for the transcript record.
    pub(crate) fn profile_name(&self) -> &'static str {
        self.profile.as_str()
    }

    /// Does this server serve `name`?
    ///
    /// A name this server does not implement at all is admitted here so that [`Server::call_tool`]'s
    /// own `unknown tool` arm answers it: telling an agent that a nonexistent tool is "withheld by
    /// the profile" would be a lie, and one that sends it looking for a flag to set.
    pub(crate) fn admits(&self, name: &str) -> bool {
        !self.withheld.iter().any(|t| t == name)
    }

    /// The `tools/list` payload for this access — [`tools_spec`] filtered by the SAME set
    /// [`ToolAccess::admits`] reads.
    fn advertised(&self) -> Value {
        let spec = tools_spec();
        let tools: Vec<Value> = spec
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|t| self.admits(t["name"].as_str().unwrap_or_default()))
            .cloned()
            .collect();
        Value::Array(tools)
    }

    /// Why `name` is not available, and what to do about it.
    ///
    /// It names the profile, the tools that ARE served, and the fact that this server cannot widen
    /// its own scope — an agent that reads "not available" without the last clause otherwise spends
    /// its next three turns hunting for the tool that turns it on.
    fn refusal(&self, name: &str) -> String {
        let by_deny = self.denied.iter().any(|t| t == name);
        let cause = if by_deny {
            format!("it was withheld by `--deny-tool {name}`")
        } else {
            format!("this server is running under the `{}` tool profile", self.profile_name())
        };
        format!(
            "tool `{name}` is not available: {cause}. Tools served in this session: {}. Only the \
             OPERATOR can widen this, by restarting the server with `--profile full` (and without \
             that `--deny-tool`) in the MCP client's launch command — this server cannot widen its \
             own scope, and asking again will not change the answer.",
            self.allowed.join(", ")
        )
    }
}

/// The OPENING line of [`instructions`] — what this server IS, and the one fact that makes the rest
/// of the text worth its context window: it is a PART.
const INSTRUCTIONS_OPENING: &str = "\
This is `vike-cli`'s agent surface: author and validate Rhai strategies, run backtests, and read \
and control one running vike-tradehub node. It is ONE surface of a larger system, and that is the \
thing no tool description can tell you.";

/// The ELSEWHERE — the operator-side commands that do what no tool here does.
///
/// ⚠ Every command named here is checked by `the_instructions_name_only_real_commands` against the
/// module that OWNS it (`crate::COMMANDS` for a verb, that command's own `USAGE` for a subcommand
/// or a flag), because a surface that names a command the binary does not have is worse than one
/// that names none: it sends an operator to a terminal to type something that fails.
/// ⚠ **Every word here is paid for by `the_instructions_stay_short_enough_to_prepend_to_every_session`'s
/// ceiling, which this text now shares with a fifth clause.** It was compressed when
/// [`INSTRUCTIONS_DELETE`] landed — the facts and every gated command spelling are unchanged, the
/// connective tissue is not. Adding a sentence here means taking one from somewhere.
const INSTRUCTIONS_ELSEWHERE: &str = "\
When a request needs another part, NAME THE COMMAND rather than refusing blind — none is a tool \
you can call; a human runs them:
- record a live tape: `vike-backend datahub --record <profile>` — the recorder is part of the data \
daemon now.
- get bars in: `vike-cli data fetch binance:BTCUSDT:1h --days 180` (public, no credentials), or \
`vike-cli data seed-demo` for a synthetic tape.
- backtest with no server: `vike-cli backtest --local --profile run.toml`.
- which venue API keys this box holds and where from: `vike-cli secrets list` / \
`vike-cli secrets path`. One store: `<project>/settings/secrets.env`.";

/// The clause the credential bullet must keep, and it is its own paragraph rather than a tail on
/// that bullet because it qualifies the whole surface.
///
/// ⚠ An OMISSION is what an agent fills in with a guess: a store's path with no such sentence
/// beside it reads as an invitation to go and use it, and this text is read by a model that may
/// hold a shell this server knows nothing about.
/// `the_mcp_surface_advertises_no_credential_writer` holds the text to it — see
/// `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`, which
/// records why the READ verbs above may be named here and the writer may not.
const INSTRUCTIONS_NO_CREDENTIAL: &str = "\
No tool on this server reads or writes a credential, and none can arm a venue.";

/// The clause the recorder and data bullets earn only when the datahub reads are SERVED — under
/// `offline` there is no `list_series`, and pointing an agent at a tool this session does not have
/// is the same defect as pointing it at a command that does not exist.
const INSTRUCTIONS_SERIES: &str = "\
A tape or a fetch lands in the history store, and `list_series` is how you confirm it arrived.";

/// The WRITE clause, added only when a write tool is served. Under `read-only` and `offline` there
/// is no write tool, and a preview gate described to an agent that cannot reach it is noise.
///
/// ⚠ The SECOND sentence is here because the same run that forced this text also placed a real
/// order under an invented venue (`venue: "node"`), and a roster of tools cannot say where a venue
/// name comes from — `submit_order`'s schema says `venue` is a required string and nothing says a
/// string is not enough. The gate now refuses it ([`Server::vet_commanded_venue`]); this is the
/// half that stops an agent spending a turn discovering that.
const INSTRUCTIONS_WRITES: &str = "\
Writes are PREVIEW-GATED: without the `preview_token` this server issued for that exact command, \
a call returns a preview and sends nothing. A `venue` must be one the node MOUNTS — read it from \
`node_snapshot`'s `venues[].venue`, never infer it.";

/// The NODE-READ clause, added when the per-call node reads are served — every ring but `offline`.
///
/// ⚠ It exists because the three node reads look interchangeable in a tool list and are not, and
/// the failure that shape produces is a WRONG ANSWER rather than a refusal: an agent asked why a
/// strategy is not trading reaches for `node_snapshot`, finds a book with no orders in it and
/// nothing anywhere about mounts, and reports the node idle. Nothing in a per-tool description can
/// say "the question you are asking belongs to a different tool on this list".
///
/// ⚠ **It is two lines long because the length is GATED** —
/// `crates/vike-ops/tests/mcp_instructions_gate.rs`'s
/// `the_instructions_stay_short_enough_to_prepend_to_every_session` caps the union of these
/// literals, which every client prepends to every session. So what each read RETURNS stays in its
/// own tool description, where a client shows it beside the tool; the only thing that cannot live
/// there, and therefore the only thing here, is the CONTRAST.
const INSTRUCTIONS_NODE_READ: &str = "\
`node_snapshot` is the BOOK, `strategy_status` is WHAT IS MOUNTED, `settings_show` is WHAT IT WAS \
CONFIGURED WITH — the wrong one reads as an empty answer, not as a wrong tool.";

/// The LIFECYCLE clause, added when a [`LIFECYCLE_TOOLS`] member is served. Under `read-only` and
/// `offline` there is none, and describing a gate an agent cannot reach is noise.
///
/// ⚠ The last sentence is the one that had to be written down. `policy_confirm` is the only
/// argument on this whole surface an agent is told to obtain from a HUMAN and forbidden to derive
/// — it holds `key` in hand, and copying it across would work every time — so the instruction has
/// to say why the asking is the point, or it reads as a field somebody forgot to default.
const INSTRUCTIONS_LIFECYCLE: &str = "\
`mount_strategy` / `unmount_strategy` / `set_setting` change the node's CONFIGURATION, not its \
book. A policy.toml `set_setting` moves a RISK CEILING and needs `policy_confirm` — the OPERATOR's \
retyping, never yours.";

/// The DELETE clause, added only when `delete_series` is served.
///
/// ⚠ **This clause REPLACES a proposed exclusion, and the reversal is why it says what it says.**
/// The original design forbade the instructions from naming any destroying subcommand at all, on
/// the argument that a model handing an operator the command has caused the deletion at one remove.
/// The owner lifted the exclusion on 2026-09-07: an agent that CAN call the tool must be told how it
/// works, and telling it less does not make the tool safer — it makes an under-briefed model likelier
/// to call it wrongly.
///
/// Three facts, and each is one a tool description cannot carry alone: that the grant is
/// irreversible in a way an order is not, that this surface asks MORE of a delete than the CLI does,
/// and that the TOKEN is the gate rather than `confirm` — the last of which
/// `every_write_touching_prompt_teaches_the_token_not_just_confirm` will fail a text that omits.
const INSTRUCTIONS_DELETE: &str = "\
`delete_series` is IRREVERSIBLE and its window may not be re-fetchable. It needs `produced_by` on \
EVERY call (more than the CLI's `rm` asks), and executes only on a second call carrying `confirm` \
AND that plan's `preview_token`.";

/// The CLOSING line — the two failure modes this text exists to prevent, stated as rules.
const INSTRUCTIONS_CLOSING: &str = "\
Never invent a tool name, and never report an operator-side command as something you ran.";

/// The MCP `instructions` field: free text a client shows the model as guidance about this server.
///
/// # Why it exists at all
///
/// A tool description can only describe ITS tool. Nothing in a roster of tools can say *what this
/// server is not*, and the measurement that forced this said so plainly: in the 2026-09-06
/// model-in-the-loop run (`.github/workflows/agent-eval.yml`, driver `claude-cli`) the agent
/// answered four cases WELL — it reported the datahub unreachable, enumerated what its toolset
/// covers, and told the operator the missing capability was a separate step — and failed all four
/// on the same shape: it never named the binary that does the thing, because nothing it could see
/// says that binary exists. An agent that must refuse and can also DIRECT is strictly more useful
/// than one that refuses blind, and this is the only channel in the protocol that carries it.
///
/// # Why it is this short
///
/// It is prepended to every session's context, so it is guidance and not a manual. It therefore
/// carries exactly what a tool description CANNOT: the fact that this is one part of a system, one
/// line per capability that lives outside it, and two rules about answering. It describes no tool
/// (the roster already does, better), teaches no procedure (`skills/` does, and those are rendered
/// from the code by `scripts/gen_skills.sh`), and repeats no argument name. Everything here is a
/// fact an agent cannot obtain from `tools/list` at any length.
///
/// # Scoping
///
/// ⚠ Scoped by the SAME [`ToolAccess`] the roster and the router are — telling a `read-only` agent
/// about a preview gate it cannot reach, or an `offline` one about a series list it does not have,
/// is the identical defect as advertising a withheld tool. The operator-side commands are NOT
/// scoped and must not be: they are things a HUMAN runs, and no profile of this server changes what
/// the operator can type.
fn instructions(access: &ToolAccess) -> String {
    let mut parts = vec![INSTRUCTIONS_OPENING, INSTRUCTIONS_ELSEWHERE, INSTRUCTIONS_NO_CREDENTIAL];
    if access.admits("list_series") {
        parts.push(INSTRUCTIONS_SERIES);
    }
    // The per-call node reads, keyed on one of the two: they share a ring (both declare
    // `openWorldHint`, neither is a write), so either name answers for the clause.
    if access.admits("strategy_status") {
        parts.push(INSTRUCTIONS_NODE_READ);
    }
    if WRITE_TOOLS.iter().any(|t| access.admits(t)) {
        parts.push(INSTRUCTIONS_WRITES);
    }
    if LIFECYCLE_TOOLS.iter().any(|t| access.admits(t)) {
        parts.push(INSTRUCTIONS_LIFECYCLE);
    }
    if access.admits("delete_series") {
        parts.push(INSTRUCTIONS_DELETE);
    }
    parts.push(INSTRUCTIONS_CLOSING);
    parts.join("\n\n")
}

/// A tool call that did not produce a result, and WHETHER A GATE SAID SO.
///
/// ⚠ The `refused` bit exists for the transcript: "the agent was refused" and "the tool broke" are
/// different facts, and a record that spelled them the same would be useless for the question it is
/// kept for. It is carried on the error rather than sniffed out of the message afterwards, because
/// a string match on prose is a gate a reworded sentence silently disarms.
///
/// `From<String>` is load-bearing exactly as `crate::exit::CliError`'s is: every tool helper in this
/// file returns `Result<_, String>`, so an unconverted `?` still compiles and still lands on the
/// non-refusal rung — which is what let this classification be added without rewriting ten arms.
#[derive(Debug)]
pub(crate) struct ToolError {
    /// What the client is told.
    pub(crate) message: String,
    /// Did a GATE refuse this (the profile, or the preview-token binding)?
    pub(crate) refused: bool,
}

impl From<String> for ToolError {
    fn from(message: String) -> Self {
        Self { message, refused: false }
    }
}

impl ToolError {
    /// A GATE said no. Nothing was attempted.
    fn refused(message: String) -> Self {
        Self { message, refused: true }
    }
}

/// How long an issued preview stays confirmable. Mirrors the Telegram surface's
/// `CONFIRM_WINDOW_MS`: long enough for an agent to read a verdict and decide, short enough that a
/// preview taken against a stale book cannot be confirmed against a moved one.
const PREVIEW_WINDOW: Duration = Duration::from_secs(60);

/// WHAT a preview was issued FOR.
///
/// ⚠ It became an enum when the surface grew a write that is not a node command. `delete_series`
/// removes stored history through a datahub; there is no `WireCommand` for it and there must not
/// be — `crate::cmd::verbs`'s vocabulary is the trade REPL's and the MCP surface's ONE construction
/// site for NODE commands, and widening it to carry a store operation would put a verb in it that
/// the REPL has no business spelling.
///
/// The alternative — a second token store beside [`PendingPreviews`] — was rejected for the reason
/// that module's doc gives about the roster: two stores means two windows, two single-use rules and
/// two binding compares, and the day they disagree is the day a token confirms something it did not
/// preview.
#[derive(Debug, Clone, PartialEq)]
enum PreviewIntent {
    /// A node command — orders and node lifecycle alike, through `crate::cmd::verbs`.
    Node(Box<WireCommand>),
    /// A stored-series DELETION, through a datahub. See [`DeleteIntent`].
    Delete(DeleteIntent),
}

/// The `delete_series` tool's intent: WHICH series, and under WHICH provenance assertion.
///
/// ⚠ `produced_by` is part of the INTENT and not a side condition, so a token previewed under one
/// assertion cannot confirm a delete under another — which is the whole point of binding a token to
/// what it previewed, applied to the one argument that decides what actually goes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DeleteIntent {
    kind: String,
    venue: String,
    symbol: Option<String>,
    group: Option<String>,
    interval: Option<String>,
    produced_by: String,
}

/// A preview that was issued and not yet consumed by a confirming call.
struct PendingPreview {
    intent: PreviewIntent,
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
fn same_intent(a: &PreviewIntent, b: &PreviewIntent) -> bool {
    fn without_id(c: &PreviewIntent) -> PreviewIntent {
        let mut c = c.clone();
        if let PreviewIntent::Node(cmd) = &mut c
            && let WireCommand::Submit(o) = cmd.as_mut()
        {
            o.client_order_id = String::new();
        }
        c
    }
    // ⚠ A DELETE intent is compared WHOLE — there is no minted field to exclude, and every one of
    // its parts changes what goes: the four identity dimensions AND the provenance assertion.
    without_id(a) == without_id(b)
}

impl PendingPreviews {
    /// Mint a token for `intent` and remember the binding.
    fn issue(&mut self, intent: PreviewIntent) -> String {
        self.by_token.retain(|_, p| p.issued.elapsed() <= PREVIEW_WINDOW);
        self.next += 1;
        let token = format!("pv-{}", self.next);
        self.by_token.insert(token.clone(), PendingPreview { intent, issued: Instant::now() });
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
/// other verb's — and so, deliberately, does the ONE startup refusal added since: a `--trace` that
/// cannot resolve a project directory (see [`resolve_trace`]). It is a request that cannot be
/// honoured, which is what the usage rung means; it is not a per-call failure, because no client
/// has connected yet. The only other failure is [`serve`]'s `io::Result` — a stdio read or write that
/// broke — which is the ordinary "it ran and failed" rung by definition, and is what
/// `ExitCode::FAILURE` already spells.
pub fn run(
    args: impl Iterator<Item = String>,
    policy_max_notional: Option<f64>,
    keys: &NodeKeyring,
    state_dir: Option<&Path>,
    datahub_keys: Option<vike_datahub_client::NodeKeys>,
) -> ExitCode {
    let config = match parse_config(args) {
        Ok(c) => c,
        Err(msg) => return args::exit_for_parse_error("mcp", USAGE, &msg),
    };
    let trace = match resolve_trace(config.trace, state_dir) {
        Ok(t) => t,
        Err(msg) => return args::exit_for_parse_error("mcp", USAGE, &msg),
    };
    if let Some(trace) = &trace {
        // Retention first, then SAY WHERE IT WENT — on stderr, because this process's stdout is the
        // JSON-RPC stream. An operator who asked for a record and is not told the path has to go
        // looking for it after the incident rather than before.
        trace.prune(mcp_trace::DEFAULT_MAX_TRACE_FILES);
        eprintln!("vike-cli mcp: recording the agent transcript under {}", trace.dir().display());
    }
    let mut server = Server {
        datahub_addr: config.datahub_addr,
        backtest_addr: config.backtest_addr,
        node_addr: config.node_addr,
        control: None,
        observe: None,
        caps: verbs::guardrail_caps(policy_max_notional),
        keys: keys.clone(),
        coids: verbs::coid_minter(),
        pending: PendingPreviews::default(),
        last_backtest: None,
        access: config.access,
        trace,
        // From the dispatcher's one env sweep — see the field, and `crate::Resolved::datahub_keys`
        // for why this is the only place that read can happen.
        datahub_keys,
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
        backtest_addr: DEFAULT_BACKTEST_ADDR.to_string(),
        node_addr: None,
        control: None,
        observe: None,
        caps: verbs::GuardrailCaps::default(),
        keys: NodeKeyring::default(),
        coids: verbs::coid_minter(),
        pending: PendingPreviews::default(),
        last_backtest: None,
        // The DEFAULT scope and NO transcript — so this constructor still describes exactly the
        // server every test that predates those two features was written against.
        access: ToolAccess::full(),
        trace: None,
        datahub_keys: None,
    }
}

/// A [`test_server`] scoped to one profile by NAME, for the transport tests in
/// `crates/vike-cli/tests/mcp_transcript.rs` — a separate crate, which cannot name [`Profile`].
///
/// Panics on an unknown name rather than falling back, for the same reason [`Profile::parse`]
/// refuses one: a test that silently ran under `full` while claiming to prove `read-only` would be
/// the worst possible failure of this suite.
pub fn test_server_under(profile: &str) -> Server {
    let profile = Profile::parse(profile).expect("the test names a real profile");
    Server { access: ToolAccess::new(profile, Vec::new()), ..test_server() }
}

/// Parse `--addr <datahub>` (default [`DEFAULT_ADDR`]), the optional `--node <host:port>` (the
/// vike-tradehub node the trade tools control), the tool SCOPE (`--profile`, repeatable
/// `--deny-tool`) and the transcript request (`--trace` / `--trace-dir`), via the shared
/// [`crate::cmd::args`] glue. A `--help`/`-h` short-circuits through [`args::help_requested`];
/// [`args::exit_for_parse_error`] in [`run`] is what turns that back into a stdout usage and an
/// exit 0.
///
/// ⚠ Both scope flags REFUSE a name they do not recognise rather than ignoring it, and that is the
/// same rule twice: a mistyped `--profile` must not serve more than the operator asked for, and a
/// mistyped `--deny-tool` must not leave them believing a tool was withheld when nothing was.
/// `--trace` and `--trace-dir` are last-one-wins, the ordinary CLI expectation for two spellings of
/// one destination.
fn parse_config(args: impl Iterator<Item = String>) -> Result<Config, String> {
    let mut addr = DEFAULT_ADDR.to_string();
    let mut backtest_addr = DEFAULT_BACKTEST_ADDR.to_string();
    let mut node: Option<String> = None;
    let mut profile = Profile::Full;
    let mut denied: Vec<String> = Vec::new();
    let mut trace = TraceRequest::Off;
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--addr" => addr = flags.value(&flag, inline)?,
            // ⚠ A SECOND address flag since ruling 7 split the served surface: `--addr` still names
            // the DATA daemon (list_series, coverage, backfill) and this one names the COMPUTE
            // daemon (run_backtest, run_sweep, run_walk_forward, list_strategies). One MCP server
            // holds tools on both planes, so it needs both.
            "--backtest-addr" => backtest_addr = flags.value(&flag, inline)?,
            "--node" => node = Some(flags.value(&flag, inline)?),
            "--profile" => profile = Profile::parse(&flags.value(&flag, inline)?)?,
            // ⚠ A deny name is validated against the SERVED roster here rather than being applied
            // blindly: a `--deny-tool submit-order` (hyphen) that quietly denied nothing would
            // leave an operator believing a write tool was withheld when it was not.
            "--deny-tool" => {
                let name = flags.value(&flag, inline)?;
                if !served_tool_names().contains(&name) {
                    return Err(format!(
                        "unknown --deny-tool {name:?} — this server serves: {}",
                        served_tool_names().join(", ")
                    ));
                }
                denied.push(name);
            }
            "--trace" => {
                args::no_value(&flag, inline)?;
                trace = TraceRequest::ProjectState;
            }
            "--trace-dir" => trace = TraceRequest::Dir(PathBuf::from(flags.value(&flag, inline)?)),
            "-h" | "--help" => return args::help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Config {
        datahub_addr: addr,
        backtest_addr,
        node_addr: node,
        access: ToolAccess::new(profile, denied),
        trace,
    })
}

/// What [`parse_config`] resolved: the THREE addresses (data daemon, compute daemon, node), the
/// tool scope, and whether a transcript was
/// asked for. A struct rather than a tuple because the tuple was already two elements and this
/// change would have made it four positional values with two `Option`s among them.
#[derive(Debug)]
struct Config {
    datahub_addr: String,
    /// The COMPUTE daemon the `Run*`/`list_strategies` tools dial (ruling 7) — a separate address
    /// from `datahub_addr` because this server holds tools on both planes.
    backtest_addr: String,
    node_addr: Option<String>,
    access: ToolAccess,
    trace: TraceRequest,
}

/// Whether the operator asked for an agent transcript, and where.
///
/// Three states rather than an `Option<PathBuf>`, because "the default location" cannot be resolved
/// by the parser: it is the composition root's already-resolved state directory, and re-deriving it
/// here would be a second walk (see [`resolve_trace`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum TraceRequest {
    /// No `--trace`: nothing is written and no directory is created. The default.
    Off,
    /// `--trace`: `<project>/settings/state/agent`, beside the change journal.
    ProjectState,
    /// `--trace-dir <dir>`: exactly there.
    Dir(PathBuf),
}

/// Turn a [`TraceRequest`] into a writer, or into the reason it cannot be honoured.
///
/// ⚠ **A `--trace` that cannot resolve a project REFUSES the process rather than starting silently
/// without a record.** That is the opposite of `vike_boot::journal_boot_settings`, which records
/// nothing when no project is above the working directory — and the difference is who asked: an
/// anchor nobody requested is right to be silent, while an operator who typed `--trace` and got a
/// server with no transcript would find out after the incident that there was nothing to read. The
/// message names the two ways out, so the refusal is actionable rather than merely correct.
fn resolve_trace(
    request: TraceRequest,
    state_dir: Option<&Path>,
) -> Result<Option<McpTrace>, String> {
    match request {
        TraceRequest::Off => Ok(None),
        TraceRequest::Dir(dir) => Ok(Some(McpTrace::new(dir))),
        TraceRequest::ProjectState => match state_dir {
            Some(dir) => Ok(Some(McpTrace::in_state_dir(dir))),
            None => Err(
                "--trace: no project directory above the working directory, so there is nowhere \
                 to write the agent transcript. Run from inside a project, set $VIKE_SETTINGS_DIR, \
                 or name a directory outright with --trace-dir <dir>."
                    .to_string(),
            ),
        },
    }
}

/// Every tool name this server implements, in [`tools_spec`] order — the roster `--deny-tool`
/// validates against and the refusal message quotes.
fn served_tool_names() -> Vec<String> {
    tools_spec()
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default().to_string())
        .collect()
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
                        // Free text the client shows the model as guidance about this server —
                        // the ONE channel that can say what this surface is NOT. Scoped by the
                        // same [`ToolAccess`] the roster is; see [`instructions`].
                        "instructions": instructions(&self.access),
                    }),
                ))
            }
            "notifications/initialized" => None,
            "ping" => Some(rpc_result(id?, json!({}))),
            // ⚠ The advertised roster is the SAME derivation the router refuses on — see
            // [`ToolAccess`]. Answering this from `tools_spec()` directly is exactly the drift the
            // type exists to make impossible.
            "tools/list" => Some(rpc_result(id?, json!({ "tools": self.access.advertised() }))),
            "resources/list" => {
                Some(rpc_result(id?, json!({ "resources": resources_spec_for(&self.access) })))
            }
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
                    render_prompt(name, arguments, &self.access)
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
                // ⚠ THE TRANSCRIPT, classified from the SAME value the response is built from and
                // BEFORE that value is consumed — so the record cannot describe an outcome
                // different from the one the client was told. A no-op when `--trace` was not asked
                // for, and never a `println!` at any point: this stdout is the protocol.
                self.trace_call(name, arguments, &outcome);
                Some(match outcome {
                    Ok(Ok(structured)) => rpc_result(id, tool_ok(structured)),
                    Ok(Err(e)) => rpc_result(id, tool_err(&e.message)),
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
    fn call_tool(&mut self, name: &str, args: &Value) -> Result<Value, ToolError> {
        // ── THE SCOPE GATE ───────────────────────────────────────────────────────────────────
        // FIRST, before any argument is parsed and before any socket is opened: a tool this server
        // does not serve must not be half-executed. One predicate, and it is the same one
        // `tools/list` filtered on, so the roster an agent read and the roster this router honours
        // cannot disagree — see [`ToolAccess`].
        if !self.access.admits(name) {
            return Err(ToolError::refused(self.access.refusal(name)));
        }
        match name {
            "validate_strategy" => Ok(tool_validate_strategy(args)?),
            "discover_params" => Ok(tool_discover_params(args)?),
            "list_templates" => Ok(tool_list_templates()),
            "list_indicators" => Ok(tool_list_indicators(args)?),
            "run_backtest" => {
                let report = tool_run_backtest(&self.backtest_addr, args)?;
                // Remember it so `vike://backtest/last` has something to serve — the resource is a
                // second WAY IN to this answer, not a second source of it. Recorded only on
                // success: a failed run must not replace the last report that actually completed.
                self.last_backtest = Some(render(&report));
                Ok(report)
            }
            "run_sweep" => Ok(tool_run_sweep(&self.backtest_addr, args)?),
            "run_walk_forward" => Ok(tool_run_walk_forward(&self.backtest_addr, args)?),
            "list_strategies" => Ok(tool_list_strategies(&self.backtest_addr)?),
            "list_series" => Ok(tool_list_series(&self.datahub_addr)?),
            // ⚠ A NAMED arm ABOVE the `is_write_tool` guard, and both halves of that sentence are
            // load-bearing. It is in [`WRITE_TOOLS`] — so it inherits the `destructiveHint`, the
            // `read-only` withholding and the transcript's write classification by construction —
            // but it is NOT a node command: it removes stored history through a datahub, and
            // `crate::cmd::verbs`'s `wire_command_for` has no spelling for it and must not grow one.
            // `the_data_deleter_keeps_every_guard` is what holds the pairing.
            "delete_series" => self.tool_delete_series(args),
            "node_snapshot" => Ok(self.tool_node_snapshot()?),
            "strategy_status" => Ok(self.tool_strategy_status()?),
            "settings_show" => Ok(self.tool_settings_show()?),
            // The write tools: build the WireCommand through the SHARED `crate::cmd::verbs`
            // construction site the trade REPL resolves through — all ten of them, orders and node
            // lifecycle alike — then gate on `confirm`. The guard reads [`WRITE_TOOLS`] rather than
            // re-spelling it as an alternative pattern, so this arm cannot drift from the roster
            // the annotations and the transcript harness read — see
            // `the_write_arm_and_the_write_roster_are_the_same_set`.
            n if is_write_tool(n) => {
                let cmd = verbs::wire_command_for(name, args)?;
                // MINT the client-order-id when the agent did not supply one — the node REFUSES a
                // remote `Submit` with an empty `client_order_id`, so `submit_order` without the
                // optional argument used to be un-executable. Minted BEFORE the confirm branch so
                // the preview shows a real id; a `confirm: true` call mints its own (the preview
                // note says so, and `client_order_id` in the accepted response is authoritative).
                let cmd = verbs::fill_client_order_id(cmd, &mut self.coids);
                // ── THE VENUE GATE ───────────────────────────────────────────────────────────
                // BEFORE the confirm branch, so it covers the PREVIEW and the CONFIRM with one
                // check and so a token is never minted for a command that cannot route. See
                // [`Server::vet_commanded_venue`] for the mechanism this closes.
                let venue_check = self.vet_commanded_venue(&cmd)?;
                // ── THE TYPED-CONFIRM GATE ───────────────────────────────────────────────────
                // Beside the venue gate and for the same structural reason: BEFORE the confirm
                // branch, so a `policy.toml` write with no operator retyping is refused rather
                // than handed an approving-looking preview and a token it could confirm with.
                // See [`typed_confirm_verdict`], which also argues what it leaves to the node.
                typed_confirm_verdict(&cmd).map_err(ToolError::refused)?;
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
                    let issued = self.pending.issue(PreviewIntent::Node(Box::new(cmd.clone())));
                    return Ok(preview_of(
                        name,
                        &cmd,
                        reason.as_deref(),
                        self.caps,
                        &issued,
                        node,
                        venue_check,
                    ));
                };
                let Some(previewed) = self.pending.take(token) else {
                    return Err(ToolError::refused(format!(
                        "preview_token {token:?} is unknown or already used — a token fires at most \
                         once. Call this tool WITHOUT `confirm` to get a fresh preview, then confirm \
                         with the `preview_token` it returns."
                    )));
                };
                if previewed.issued.elapsed() > PREVIEW_WINDOW {
                    return Err(ToolError::refused(format!(
                        "preview_token {token:?} EXPIRED (previews stay confirmable for {}s). The \
                         book may have moved since it was priced — take a fresh preview.",
                        PREVIEW_WINDOW.as_secs()
                    )));
                }
                let rebuilt = PreviewIntent::Node(Box::new(cmd.clone()));
                if !same_intent(&previewed.intent, &rebuilt) {
                    return Err(ToolError::refused(
                        "preview_token does not match this command — it was issued for a DIFFERENT \
                         one, and a token is bound to what it previewed. Preview the command you \
                         intend to send, then confirm with THAT token."
                            .to_string(),
                    ));
                }
                // ⚠ Execute the PREVIEWED command, not the rebuilt one. They express the same
                // intent (checked above) but only the stored one carries the client_order_id the
                // preview displayed and the node dry-ran — so what reaches the venue is exactly
                // what was shown, id included.
                let PreviewIntent::Node(previewed_cmd) = &previewed.intent else {
                    // Unreachable: `same_intent` above compares the DISCRIMINANT too, so a Delete
                    // intent cannot have matched a Node one. Named rather than unwrapped, because
                    // an `expect` here would be a claim about a match two branches away.
                    return Err(ToolError::refused(
                        "preview_token was issued for a different KIND of operation".to_string(),
                    ));
                };
                let mut answered = self.execute(previewed_cmd, reason)?;
                // ...and the venue disposition rides the ANSWER too, not the preview alone. The
                // write that actually happened is the one whose record has to say whether its
                // venue was compared against the node's mounted set, because
                // [`VENUE_CHECK_UNVERIFIED`] is a real ALLOW path: a transcript showing only
                // `outcome: "accepted"` cannot tell an operator whether the gate ran. Stamped
                // here rather than inside [`Server::execute`] so the one site that COMPUTES the
                // verdict is the one site that reports it, and on every answered shape (an
                // `outcome: "unknown"` write may still execute, so it needs the disposition just
                // as much as an accepted one does).
                if let Some(obj) = answered.as_object_mut() {
                    obj.insert("venue_check".to_string(), json!(venue_check));
                }
                Ok(answered)
            }
            // A test-only tool that panics, pinning the catch_unwind guard in `handle` (mirrors
            // the vike-mcp `rpc.rs` fake host's "panic" tool). Never advertised in `tools_spec`.
            #[cfg(test)]
            "__test_panic" => panic!("kaboom"),
            // ⚠ NOT a refusal: a name this server does not implement is a client mistake, and the
            // scope gate above deliberately let it through so it lands here rather than being
            // reported as "withheld by the profile" — which would send an agent looking for a flag
            // that would not help.
            other => Err(ToolError::from(format!("unknown tool: {other}"))),
        }
    }

    /// `delete_series` — DELETE stored history through a datahub, IRREVERSIBLY.
    ///
    /// # ⚠ Why this tool exists at all, and what the guards had to clear
    ///
    /// The design that proposed it opened by EXCLUDING it: a model must not delete data. The owner
    /// reversed that on 2026-09-07 — a user must be able to delete through both the CLI and this
    /// surface — and the exclusion was replaced by GATING rather than by nothing. The argument the
    /// exclusion rested on is what sets the bar: an irreversible delete of market history is a
    /// larger grant than an order, and in one way larger than a credential write — a credential can
    /// be reissued at the venue, and a deleted tape whose venue no longer serves that window cannot
    /// be re-fetched at all.
    ///
    /// So it gets every guard an order gets, plus one an order does not:
    ///
    /// 1. [`WRITE_TOOLS`] membership — the mandatory preview, the `destructiveHint`, the
    ///    `read-only` withholding and the transcript's write classification, all by construction;
    /// 2. the single-use, expiring, INTENT-BOUND `preview_token`;
    /// 3. **`produced_by` is REQUIRED, unconditionally** — see below;
    /// 4. withheld from `read-only` (it is a write) and from `offline` (it opens a socket).
    ///
    /// # ⚠ Guard 3: provenance is UNCONDITIONAL here, unlike on the CLI
    ///
    /// `vike-cli data rm` requires `--produced-by` only for a SWEEP, on the argument that deleting
    /// one fully-named series is byte-for-byte what the Data Manager's Delete already does behind a
    /// confirm modal. **That argument does not hold on this surface.** A human naming a series has
    /// SEEN it — the GUI's Delete is reached by clicking a row rendered from the store — while a
    /// model composes the four dimensions from context that may be stale, summarised, or its own
    /// earlier output. A provenance assertion is the one check that fails on a plausible-but-wrong
    /// identity, because a wrongly-named series will not carry the asserted key. It converts the act
    /// from "delete what I named" to "delete what I named, and prove it is what I think it is", and
    /// it costs an agent nothing it should not already have: a model that cannot say which producer
    /// wrote the rows it wants gone does not know enough to delete them.
    ///
    /// There is no override. The CLI's own refusal of a `--force` escape applies here with more
    /// force, not less.
    ///
    /// # The two ways this reaches a server that will not serve it
    ///
    /// ⚠ This heading read "DECLARED GAP: this crate cannot authenticate to a datahub" until #1691,
    /// which closed exactly that: [`Server::datahub_keys`] carries the pair when the box sets it, so
    /// this tool AUTHENTICATES. What remains is not a gap but two legible refusals with different
    /// remedies — this server holding no key (dials unauthenticated, a keyed datahub refuses the
    /// handshake), or the DATAHUB holding none (advertises no delete verb, so
    /// `DatahubClient::delete_series` refuses before a frame is sent). [`DELETE_REMOTE_HINT`] is the
    /// sentence that separates them for whoever hit one.
    fn tool_delete_series(&mut self, args: &Value) -> Result<Value, ToolError> {
        use vike_datahub_client::proto::SeriesSelector;

        let intent = delete_intent_from(args).map_err(ToolError::refused)?;
        let selector = SeriesSelector {
            kind: intent.kind.clone(),
            venue: intent.venue.clone(),
            symbol: intent.symbol.clone(),
            group: intent.group.clone(),
            interval: intent.interval.clone(),
        };
        let reason = verbs::reason_from_tool_args(args);
        let token = args.get("preview_token").and_then(Value::as_str);
        let confirmed = args.get("confirm").and_then(Value::as_bool) == Some(true);

        // ⚠ The token is consumed BEFORE the socket is opened, so a stale or mismatched one is
        // refused without a round trip — and, more importantly, without the server ever being asked
        // to plan a deletion nobody may confirm.
        let approved = match token.filter(|_| confirmed) {
            None => None,
            Some(token) => {
                let Some(previewed) = self.pending.take(token) else {
                    return Err(ToolError::refused(format!(
                        "preview_token {token:?} is unknown or already used — a token fires at \
                         most once. Call this tool WITHOUT `confirm` to get a fresh plan, then \
                         confirm with the `preview_token` it returns."
                    )));
                };
                if previewed.issued.elapsed() > PREVIEW_WINDOW {
                    return Err(ToolError::refused(format!(
                        "preview_token {token:?} EXPIRED (previews stay confirmable for {}s). The \
                         store may have changed since it was planned — take a fresh plan.",
                        PREVIEW_WINDOW.as_secs()
                    )));
                }
                if !same_intent(&previewed.intent, &PreviewIntent::Delete(intent.clone())) {
                    return Err(ToolError::refused(
                        "preview_token does not match this deletion — it was issued for a \
                         DIFFERENT selector or a DIFFERENT `produced_by`, and a token is bound to \
                         what it previewed. Plan the deletion you intend, then confirm with THAT \
                         token."
                            .to_string(),
                    ));
                }
                Some(previewed)
            }
        };

        // ⚠ ALWAYS the dry run first, confirmed or not — and its FAILURE does not fail a PREVIEW.
        //
        // That degrade is the same one `Server::node_preview` already makes for an unreachable
        // node, and it is made for the same reason: a preview that could not ask must SAY it could
        // not ask, rather than turning into an error an agent reads as "the tool is broken". A
        // token minted over an unasked store deletes nothing — the confirming call dials again and
        // fails identically — and [`delete_preview`]'s note says so in the same words `preview_of`
        // uses for an unasked node.
        //
        // A CONFIRMING call is the opposite: it must fail, loudly, because it was going to act.
        let asked = self.datahub_for_delete().and_then(|mut client| {
            client
                .delete_series(&selector, Some(&intent.produced_by), true)
                .map_err(|e| ToolError::from(format!("{e}\n{DELETE_REMOTE_HINT}")))
        });

        let Some(_approved) = approved else {
            let issued = self.pending.issue(PreviewIntent::Delete(intent.clone()));
            let (plan, plan_error) = match asked {
                Ok(planned) => (Some(planned.plan), None),
                Err(e) => (None, Some(e.message)),
            };
            return Ok(delete_preview(
                &intent,
                plan.as_ref(),
                plan_error.as_deref(),
                reason.as_deref(),
                &issued,
            ));
        };
        // Past the gate: the plan pass had to succeed, because the delete pass is about to run
        // against the same server.
        asked?;
        let mut client = self.datahub_for_delete()?;
        let done = client
            .delete_series(&selector, Some(&intent.produced_by), false)
            .map_err(|e| ToolError::from(e.to_string()))?;
        let outcome = done.outcome.unwrap_or_default();
        Ok(json!({
            "will_execute": true,
            "deleted": outcome.deleted.len(),
            "failed": outcome.failed.len(),
            "matched": done.plan.matched(),
            "rows": done.plan.rows(),
            "bytes": done.plan.bytes(),
            "plan": done.plan.lines(),
            "failures": outcome.failed.iter().map(|(id, why)| json!({
                "series": vike_datahub_client::proto::describe_id(id),
                "error": why,
            })).collect::<Vec<_>>(),
            "reason": reason,
            "note": "DELETED — irreversible. A non-empty `failures` list is a PARTIAL run: one \
                     broken series is one skipped series, the rest went, and re-running finishes \
                     the job (the delete is idempotent).",
        }))
    }

    /// The datahub connection `delete_series` needs — AUTHENTICATED when this server holds datahub
    /// keys, and a legible refusal when it does not.
    ///
    /// See [`Server::datahub_keys`] for WHEN `None` happens — a box that set neither datahub key —
    /// and what the unauthenticated fallback then reaches.
    fn datahub_for_delete(&self) -> Result<DatahubClient, ToolError> {
        let addr = &self.datahub_addr;
        match &self.datahub_keys {
            Some(keys) => {
                DatahubClient::connect_authed(addr, keys, vike_datahub_client::Scope::Control)
                    .map_err(|e| {
                        ToolError::from(format!("cannot connect to datahub at {addr}: {e}"))
                    })
            }
            None => DatahubClient::connect(addr).map_err(|e| {
                ToolError::from(format!(
                    "cannot connect to datahub at {addr}: {e}\n{DELETE_REMOTE_HINT}"
                ))
            }),
        }
    }

    /// Append ONE `tools/call` to the agent transcript, if one was asked for.
    ///
    /// ⚠ **Everything here is DERIVED from the call and its outcome** — no state is carried between
    /// calls and nothing is re-executed to find out what happened, so the record cannot describe a
    /// different call from the one the client was answered with. The classification:
    ///
    ///   * a write tool's mandatory preview is [`Verdict::Preview`], never `Ok` — "the agent called
    ///     submit_order" and "an order left this process" are different facts;
    ///   * a GATE saying no is [`Verdict::Refused`], read off [`ToolError::refused`] rather than
    ///     sniffed out of the message, because a reworded sentence must not silently reclassify;
    ///   * a panic is an [`Verdict::Error`] like any other failure — the session survives it, and so
    ///     does the record.
    ///
    /// The token identity is [`TOKEN_MINTED`] when the preview issued it and [`TOKEN_PRESENTED`]
    /// when the call supplied one; that constant's own doc argues why "spent" would be a claim this
    /// record cannot make.
    ///
    /// A failed append goes to **stderr**. It is not fatal — a server that stopped answering an
    /// agent because it could not write a log line would be trading availability for bookkeeping —
    /// and it is not silent either, which is the failure mode that makes an empty transcript
    /// indistinguishable from a quiet night.
    fn trace_call(
        &self,
        name: &str,
        args: &Value,
        outcome: &std::thread::Result<Result<Value, ToolError>>,
    ) {
        let Some(trace) = &self.trace else { return };
        let presented = args.get("preview_token").and_then(Value::as_str).map(str::to_string);
        let (verdict, detail, token, token_role) = match outcome {
            Ok(Ok(structured)) if structured["will_execute"] == json!(false) => (
                Verdict::Preview,
                None,
                structured["preview_token"].as_str().map(str::to_string),
                Some(TOKEN_MINTED),
            ),
            Ok(Ok(_)) => (Verdict::Ok, None, presented, Some(TOKEN_PRESENTED)),
            Ok(Err(e)) if e.refused => {
                (Verdict::Refused, Some(e.message.clone()), presented, Some(TOKEN_PRESENTED))
            }
            Ok(Err(e)) => {
                (Verdict::Error, Some(e.message.clone()), presented, Some(TOKEN_PRESENTED))
            }
            Err(_) => (
                Verdict::Error,
                Some(format!("tool panicked: {name}")),
                presented,
                Some(TOKEN_PRESENTED),
            ),
        };
        // A call that neither minted nor presented a token names neither — `"token": null` beside a
        // `"token_role": null` reads as "not a gated write", which is what it is.
        let (token, token_role) = match token {
            Some(t) => (Some(t), token_role),
            None => (None, None),
        };
        let call = mcp_trace::Call {
            tool: name,
            write: is_write_tool(name),
            profile: self.access.profile_name(),
            args,
            verdict,
            detail,
            token,
            token_role,
        };
        if let Err(e) = trace.append(vike_model::clock::now_ms(), &call) {
            eprintln!("vike-cli mcp: the agent transcript was NOT written: {e}");
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
        // ⚠ THE SAME SCOPE GATE `call_tool` applies, and for the same reason a resource routes into
        // a tool's implementation at all: this is a second WAY IN, so a resource served while its
        // tool is withheld would hand back exactly the answer the profile refused. [`RESOURCE_TOOLS`]
        // is the mapping, held equal to [`resources_spec`] by
        // `every_resource_is_a_way_in_to_a_named_tool`.
        if let Some((_, tool)) = RESOURCE_TOOLS.iter().find(|(u, _)| *u == uri)
            && !self.access.admits(tool)
        {
            return Err(self.access.refusal(tool));
        }
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

    /// Ask the node WHAT IT IS RUNNING — the strategy-level read verb
    /// ([`vike_tradehub_client::strategy_status`]), returned as its wire payload verbatim.
    ///
    /// ⚠ **A PER-CALL connection, not `self.observe`, and that is the verb's shape rather than a
    /// shortcut.** The held observe handle is a SUBSCRIBED push pipe — it delivers `WireSnapshot`
    /// frames and nothing else — so there is no way to ask it a question at all. The client
    /// function opens one short-lived `Scope::Observe` connection, sends the request, reads the
    /// answer and drops it, which is also why nothing here participates in
    /// [`Server::tool_node_snapshot`]'s liveness dance: a per-call read cannot serve a stale frame,
    /// because it holds no frame between calls.
    ///
    /// The OBSERVE key is the right (and only) credential: the node serves this verb read-only
    /// under either scope, and the client always connects under observe — a control key cannot
    /// substitute, since the node verifies each scope against its own key.
    ///
    /// The client refuses CLIENT-SIDE against a node whose `Welcome.features` does not advertise
    /// the strategy verbs, so nothing is sent to a node that could not decode the frame; that
    /// arrives here as `io::ErrorKind::Unsupported` and [`node_read_failure`] adds the one action
    /// that fixes it.
    fn tool_strategy_status(&self) -> Result<Value, String> {
        let (addr, key) = self.observe_read_target()?;
        match vike_tradehub_client::strategy_status(addr.as_str(), key.as_bytes()) {
            // Serialized VERBATIM, the same convention `vike-cli trade status --json` follows for
            // this payload (it nests it under a `strategy_status` key beside the trading mode,
            // precisely so the payload itself stays byte-for-byte the node's): an agent gets the
            // wire shape rather than a second hand-maintained schema that could come to disagree
            // with it.
            Ok(status) => serde_json::to_value(status)
                .map_err(|e| format!("strategy_status: the node's answer did not serialize: {e}")),
            Err(e) => Err(node_read_failure("strategy_status", &addr, &e)),
        }
    }

    /// Ask the node for its EFFECTIVE SETTINGS — the read half of the settings pair
    /// ([`vike_tradehub_client::settings_show`]), returned as its wire payload verbatim.
    ///
    /// Same per-call shape and the same observe-key rule as [`Server::tool_strategy_status`]; the
    /// capability it negotiates is the settings-SHOW one, which is deliberately separate from the
    /// settings-WRITE capability `set_setting` needs — a node may serve one and not the other, and
    /// the two refusals name different strings for that reason.
    ///
    /// ⚠ Nothing here redacts anything, and nothing here needs to: `WireSettingsRow`'s values
    /// arrive already redacted, because the node performs it in the shared builder ON
    /// CONSTRUCTION. Re-applying a rule this crate would have to keep in step with the node's is
    /// the second-authority failure, not a belt.
    fn tool_settings_show(&self) -> Result<Value, String> {
        let (addr, key) = self.observe_read_target()?;
        match vike_tradehub_client::settings_show(addr.as_str(), key.as_bytes()) {
            Ok(show) => serde_json::to_value(show)
                .map_err(|e| format!("settings_show: the node's answer did not serialize: {e}")),
            Err(e) => Err(node_read_failure("settings_show", &addr, &e)),
        }
    }

    /// The address and OBSERVE key a per-call read verb needs, or the message saying which of the
    /// two is missing. Shared by the two per-call reads so they cannot answer differently about a
    /// box that is configured the same way for both.
    fn observe_read_target(&self) -> Result<(String, String), String> {
        let addr = self.node_addr.clone().ok_or(
            "no vike-tradehub node configured — pass `--node <host:port>` to read node state",
        )?;
        let (key, _) =
            self.keys.observe().ok_or_else(|| self.missing_key(nodekeys::OBSERVE_KEY_ENV))?;
        Ok((addr, key.to_string()))
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
        // ⚠ ONE verb leaves by a different door, and the reason is the ANSWER rather than the
        // command. Everything below is fire-and-forget over the persistent worker, whose reply
        // mapping collapses every acceptance to an empty-coid `Accepted` — which is exactly right
        // for an order and throws away the one fact a settings write exists to report, whether the
        // node applied the value LIVE or only put it on disk for the next boot.
        // `vike_tradehub_client::set_setting`'s own doc calls that out as why it is synchronous.
        // The two-call preview gate above is unaffected: this is the transport for a command that
        // has already been previewed, tokenised and confirmed.
        if matches!(cmd, WireCommand::SetSetting { .. }) {
            return self.execute_settings_write(cmd, reason.as_deref());
        }
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

    /// Execute a CONFIRMED `SetSetting` — the per-call, SYNCHRONOUS control verb
    /// ([`vike_tradehub_client::set_setting`]), which is the only path that carries the node's
    /// `restart_required` back.
    ///
    /// It opens a fresh short-lived `Scope::Control` connection and drops it, so none of
    /// [`Server::execute`]'s reconnect machinery applies: there is no held handle to find dead, and
    /// a failure here is a failure of THIS attempt with nothing in flight to be uncertain about.
    /// That is a real simplification and it is worth naming — the `Disconnected` case that makes an
    /// order's outcome UNKNOWN cannot arise, because the reply is read on the same call that wrote
    /// the request.
    ///
    /// The client refuses CLIENT-SIDE against a node that does not advertise the settings-write
    /// capability, and the node itself enforces the typed-confirm contract
    /// ([`typed_confirm_verdict`] is only this side's presence half) plus the loader validation.
    /// Every one of those arrives as an `io::Error` whose text is the node's or the client's own,
    /// kept verbatim.
    fn execute_settings_write(
        &self,
        cmd: &WireCommand,
        reason: Option<&str>,
    ) -> Result<Value, String> {
        let WireCommand::SetSetting { file, key, value, confirm } = cmd else {
            // Unreachable through [`Server::execute`], which matches before calling. Answered
            // rather than panicked: a tool handler's job is to answer the client.
            return Err("execute_settings_write was handed a command that is not a settings write"
                .to_string());
        };
        let addr = self.node_addr.as_ref().ok_or(
            "no vike-tradehub node configured — pass `--node <host:port>` to enable order-write tools",
        )?;
        let (control_key, _) =
            self.keys.control().ok_or_else(|| self.missing_key(nodekeys::CONTROL_KEY_ENV))?;
        match vike_tradehub_client::set_setting(
            addr.as_str(),
            control_key.as_bytes(),
            file,
            key,
            value,
            confirm.as_deref(),
            reason,
        ) {
            Ok(restart_required) => Ok(json!({
                "sent": true,
                "outcome": "accepted",
                "restart_required": restart_required,
                "note": if restart_required {
                    "the node ACCEPTED and WROTE this key, and it is NOT in force yet: the running node keeps its boot-time value and the edit is what the next boot loads. Every policy.toml key answers this way — policy is never hot-applied. Tell the operator a restart is needed; settings_show reads the file back."
                } else {
                    "the node ACCEPTED this key and APPLIED IT LIVE — no restart is needed. settings_show reads back the effective value."
                }
            })),
            // ⚠ A refusal and a transport fault both land here, and both mean NOTHING WAS WRITTEN:
            // the node validates the would-be file with its own loader before a byte lands, and a
            // connection that failed never delivered the request. The message is the node's or the
            // client's own, kept whole — it names the key, the loader's objection or the missing
            // capability, which is what an agent has to act on.
            Err(e) => Err(format!("settings write to {addr} was NOT applied: {e}")),
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
            Err(e) => return Ok(Err(control_rejection(e))),
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

    /// Refuse a write whose venue this node does not mount — and answer which of the three
    /// [`VENUE_CHECK_MOUNTED`] dispositions the preview should report.
    ///
    /// # The defect this closes, read off the write path
    ///
    /// A venue string is carried VERBATIM from the tool argument to the core and is never compared
    /// to anything on the way: [`crate::cmd::verbs::verb_from_tool_args`] takes it as text,
    /// `vike_tradehub::server`'s `accept_command` vets only the rate token and the notional cap,
    /// and its `lower_command` copies it onto the `OrderRequest`. The node's dry-run is the same
    /// vetting (`ControlLimits::preview_vet` is `notional_reason` and nothing else), so a PREVIEW
    /// accepts it too. What then happens in the core is the part worth stating exactly, because it
    /// is not a rejection: `vike_core`'s `CoreThread::apply_intent_routed` resolves the engine with
    /// `route_of(…)` and then `unwrap_or(0)` — **a venue that routes to nothing is routed to the
    /// PRIMARY engine**, whichever venue happens to be mounted first. And because the string is not
    /// in `vike_model::VENUES`, `preflight_order_at`'s unknown-venue affordance answers `Ok(())`,
    /// so every capability check (order kind, TIF, margin mode) is SKIPPED for it in silence.
    ///
    /// So the measured failure — an agent inventing `venue: "node"` from the operator's wording,
    /// previewed, confirmed, and really resting in the book — was not a validation gap at one
    /// layer. Nothing anywhere between the tool argument and the venue edge had an opinion. On a
    /// single-venue paper mount that costs nothing; on a multi-venue live mount it books the
    /// operator's order on an account they never named, with the capability preflight off.
    ///
    /// # What this compares, and why it is the node's own answer rather than a roster
    ///
    /// The evidence is `node_snapshot`'s `venues[].venue` — the per-engine ledger blocks the node
    /// publishes — and NOT `vike_model::VENUES`. A roster check would refuse a legitimate mount:
    /// the unknown-venue affordance above exists for paper and sim engines behind non-roster ids,
    /// and a node is free to mount one. Asking the node keeps the question "does this command
    /// route" rather than "is this a venue we ship".
    ///
    /// ⚠ **The comparison is EXACT, and case-folding it would reopen the hole.** Routing compares
    /// the payload's venue to an engine's `route_key` with `==`
    /// (`vike_core`'s `engine_idx_for_route_key`), so `"Binance"` routes to nothing on a node
    /// mounting `"binance"` — and therefore falls back to the primary engine exactly as `"node"`
    /// did. A refusal looser than the routing admits strings the routing then silently redirects.
    ///
    /// ⚠ **NO EVIDENCE IS NOT A REFUSAL.** When the mounted set cannot be read the command is
    /// allowed through and the preview says so ([`VENUE_CHECK_UNVERIFIED`]) — the disposition
    /// `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md` argues, and the
    /// reason it is not reckless is that the mounted set is read over the node connection the write
    /// itself needs: a node that cannot be read is overwhelmingly a node that cannot be written to
    /// either, and [`Server::execute`] fails there anyway. The one configuration where the two come
    /// apart is a session holding a CONTROL key and no OBSERVE key, which is a declared residual
    /// rather than an oversight — every documented setup resolves both from one store.
    /// The I/O half; [`venue_verdict`] is the decision, and the split is what makes the refusal
    /// testable without a node.
    fn vet_commanded_venue(&mut self, cmd: &WireCommand) -> Result<&'static str, ToolError> {
        let venue = commanded_venue(cmd);
        // Asked ONLY when there is a venue to check, so a venue-less verb — the panic button
        // above all — costs no node round trip and cannot be delayed by one.
        let mounted = venue.and_then(|_| self.mounted_venues());
        venue_verdict(venue, mounted.as_deref()).map_err(ToolError::refused)
    }

    /// The venues this node reports mounting, or `None` when that could not be established.
    ///
    /// Read through [`Server::tool_node_snapshot`] rather than a second dial, which buys three
    /// things: the reconnect-and-liveness gate that tool already owns applies here too (a STALE
    /// frame can never answer this question), the answer is the same payload the refusal tells the
    /// agent to go and read, and there is no second way to reach the node.
    ///
    /// `None` covers every "no evidence" shape — no node configured, no observe key, a node that
    /// is down — and one more that is worth spelling out: a frame carrying an EMPTY `venues` array.
    /// That is a node that published no ledger block, not a node that mounts nothing, and reading
    /// it as the latter would refuse every write against it.
    ///
    /// ⚠ **That shape is REAL and was measured**, not a defensive hypothetical: a `vike-tradehub`
    /// daemon that has not folded anything yet publishes a frame with `seq: 0`, an `identity`
    /// block, the primary `venue` and `symbol` — and `venues: []`, because `vike_core`'s
    /// `CoreSnapshot::empty` carries no ledger blocks while `CoreSnapshot::build` always carries at
    /// least the primary one. So **this gate is inert between a node's start and its first fold**,
    /// reporting [`VENUE_CHECK_UNVERIFIED`] for every write in that window. Read off the
    /// `vike-agent-eval` transcript of a freshly started paper node, 2026-09-06, which is why that
    /// harness rests an order before the case runs.
    ///
    /// ⚠ **The top-level `venue` may NOT be used to close that window**, tempting as it is: it is
    /// the PRIMARY engine's venue, and on a multi-venue node in the same pre-fold window it is not
    /// the whole mounted set — refusing on it would reject a legitimate write to a mounted
    /// secondary while telling the operator, falsely, that the node mounts only the primary. A
    /// wrong refusal on the order path is worse than a disclosed unmade check. Closing it properly
    /// means the node publishing its mounted set in the placeholder frame, which
    /// `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md` carries as a reopen
    /// condition.
    fn mounted_venues(&mut self) -> Option<Vec<String>> {
        let snap = self.tool_node_snapshot().ok()?;
        let venues: Vec<String> = snap["venues"]
            .as_array()?
            .iter()
            .filter_map(|v| v["venue"].as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .collect();
        (!venues.is_empty()).then_some(venues)
    }

    /// The "this key is nowhere" tool error. It names BOTH sources, because the old message said
    /// "is not set in the environment" while the daemon's own keys sat unread in the credential
    /// store — an agent (and the human reading its transcript) has no way to diagnose that.
    ///
    /// ⚠ The second command it names is `backend status`, NOT `secrets list`. A node key is not in
    /// the credential store any more (`docs/decisions/0051-node-keys-live-in-their-own-store.md`),
    /// and `secrets list` deliberately does not list one — an agent told to run it would read an
    /// accurate "not there" about the wrong file and conclude the key is missing when it is not.
    fn missing_key(&self, name: &str) -> String {
        format!(
            "{name} is set neither in the process environment nor in the node-key store — \
             run `vike-cli secrets path` to see which stores this project resolves to, and \
             `vike-cli backend status` to see whether this box holds a node key"
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

fn tool_run_backtest(backtest_addr: &str, args: &Value) -> Result<Value, String> {
    let profile = args
        .get("profile")
        .and_then(Value::as_str)
        .ok_or("run_backtest requires a `profile` string argument")?;
    let mut profile_toml = profile.to_string();
    if let Some(script) = args.get("script").and_then(Value::as_str) {
        profile_toml = crate::cmd::backtest::inject_script_src(&profile_toml, script)?;
    }
    let mut client = DatahubClient::connect(backtest_addr)
        .map_err(|e| format!("cannot connect to the backtest daemon at {backtest_addr}: {e} (start it with `vike-backend backtest --addr`)"))?;
    let report_json = client.run_backtest(&profile_toml)?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server report was not valid JSON: {e}"))?;
    Ok(json!({ "report": report }))
}

/// List the compiled NATIVE backtest-strategy roster the remote `vike-backend backtest --addr` daemon advertises
/// (`vike_backtest::harness::STRATEGIES` over RPC) — the names a profile's `strategy.name` can
/// resolve, so an agent can discover which strategies exist before authoring a profile. Connects
/// to the compute daemon like `run_backtest`; a connect failure or a server-side error is a clean tool
/// error.
fn tool_list_strategies(backtest_addr: &str) -> Result<Value, String> {
    let mut client = DatahubClient::connect(backtest_addr)
        .map_err(|e| format!("cannot connect to the backtest daemon at {backtest_addr}: {e} (start it with `vike-backend backtest --addr`)"))?;
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

/// Run a remote parameter-grid search on the remote `vike-backend backtest --addr` daemon — the
/// absorbed `vike-mcp` `run_sweep`, gone remote: EXACTLY the request `vike-cli backtest` builds for
/// a profile carrying a `[sweep]` table (the SAME profile TOML over the SAME `run_sweep_profile`
/// wire verb), so the tool and the human command can never drift. The optional `rank_by` argument
/// names the server-side metric.
///
/// ⚠ **The TOOL keeps its name while the human VERB was deleted, and that is ruling 13's own
/// split.** `vike-cli sweep` is retired (`crate::RETIRED_COMMANDS`) because a second verb is a
/// second word for one operation; `RunSweep` is the PROTOCOL, not the prompt, and the ruling says
/// outright that the wire verb keeps its own name. Nothing here routed through that subcommand —
/// this dials the daemon directly — so the deletion changes no behaviour on this surface.
///
/// NOTE the trade this shares with `run_backtest`: shipping the TOML verbatim means a profile-SHAPE
/// error (no `[sweep]` table, bad range, unknown strategy) now surfaces from the SERVER rather than
/// before the connect. One parser, one error source.
fn tool_run_sweep(backtest_addr: &str, args: &Value) -> Result<Value, String> {
    let profile_toml = profile_from_args(args, "run_sweep")?;
    let rank_by = args.get("rank_by").and_then(Value::as_str);
    let mut client = DatahubClient::connect(backtest_addr)
        .map_err(|e| format!("cannot connect to the backtest daemon at {backtest_addr}: {e} (start it with `vike-backend backtest --addr`)"))?;
    let report_json = client.run_sweep_profile(&profile_toml, rank_by)?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server sweep report was not valid JSON: {e}"))?;
    Ok(json!({ "sweep": report }))
}

/// Run a remote out-of-sample walk-forward on the remote `vike-backend backtest --addr` daemon — the absorbed `vike-mcp`
/// `run_walk_forward`, gone remote: the SAME request the `walkforward` subcommand builds (the
/// profile TOML over the `run_walkforward_profile` wire verb).
///
/// The WHOLE protocol rides in the profile's `[walkforward]` table — the split count, and since the
/// optimizing driver landed, whether each window re-searches its own training half (`search`), the
/// training shape (`mode`) and how a window scores its candidates (`rank_by`). This function
/// therefore gains no argument and needs none: `crates/vike-backtest/src/compute_server.rs`'s
/// `run_walkforward_profile` reads that table and picks the driver, so a tool argument here would
/// be a second place to say it and a second thing for an agent to get wrong.
///
/// ⚠ The consequence for the ADVERTISED description (`tools_spec` below), which is what a model
/// actually reads: the default — `n_splits` alone — is still the FIXED-parameter stability walk, so
/// the description has to say both what this can do and what it did not do, or a model reports an
/// optimization it never ran. The answer distinguishes them: only a window that searched carries
/// `chosen_params`.
fn tool_run_walk_forward(backtest_addr: &str, args: &Value) -> Result<Value, String> {
    let profile_toml = profile_from_args(args, "run_walk_forward")?;
    let mut client = DatahubClient::connect(backtest_addr)
        .map_err(|e| format!("cannot connect to the backtest daemon at {backtest_addr}: {e} (start it with `vike-backend backtest --addr`)"))?;
    let report_json = client.run_walkforward_profile(&profile_toml)?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server walk-forward report was not valid JSON: {e}"))?;
    Ok(json!({ "walkforward": report }))
}

/// List every stored series the remote `vike-backend backtest --addr` daemon holds, with its cheap coverage — the
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

/// The sentence appended to every `delete_series` connection failure — what an operator can do
/// about it. See [`Server::datahub_keys`].
///
/// ⚠ **It named the wrong remedy between #1688 and #1691.** It read "this server cannot yet
/// authenticate to one" and sent the operator to the box; #1691 made that false, and a model
/// relaying it would have told a human to go SSH somewhere rather than to set the two variables
/// that fix it. The failure it is appended to has two causes with different remedies, so it now
/// separates them: this MCP server holding no key (set them where it launches), or the datahub
/// itself holding none — which advertises no delete verb at all
/// (`docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`), and no change on this side
/// helps.
const DELETE_REMOTE_HINT: &str = "The delete verb is served ONLY by a datahub that holds node keys. Either set \
     VIKE_DATAHUB_OBSERVE_KEY and VIKE_DATAHUB_CONTROL_KEY where this MCP server launches, so it \
     authenticates, or — if the datahub itself holds none, in which case it advertises no delete \
     verb at all — tell the operator to run it on that box: \
     `vike-cli data rm --kind K --venue V --produced-by PREFIX --dry-run` first, then without \
     `--dry-run` and with `--yes`.";

/// Read a `delete_series` call's arguments into a [`DeleteIntent`], refusing every shape the store
/// cannot act on.
///
/// ⚠ **`produced_by` is required HERE**, before anything is dialled and before a token is minted —
/// see [`Server::tool_delete_series`] for why it is unconditional on this surface and conditional on
/// the CLI. What is deliberately NOT checked here is anything needing the store's layout table (an
/// unknown kind, an interval on a kind that does not sub-partition by one): that is
/// `vike_data::store_kind`'s to judge, and a roster copied into this crate would be a second list
/// to keep in step — the rule `crate::cmd::data`'s own doc states for `fetch`.
fn delete_intent_from(args: &Value) -> Result<DeleteIntent, String> {
    let required = |name: &str| -> Result<String, String> {
        args.get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("`{name}` is required and must be a non-empty string"))
    };
    let optional = |name: &str| -> Result<Option<String>, String> {
        match args.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.trim().to_string())),
            // An EMPTY string is refused rather than read as "omitted": an empty `symbol=` is the
            // store's GROUPED-series sentinel, so an empty selector names neither layout. Omitting
            // the field is how a dimension is wildcarded.
            Some(_) => Err(format!(
                "`{name}` must be a non-empty string, or absent to wildcard that dimension"
            )),
        }
    };
    let kind = required("kind")?;
    let venue = required("venue")?;
    let symbol = optional("symbol")?;
    let group = optional("group")?;
    let interval = optional("interval")?;
    let produced_by = args
        .get("produced_by")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(
            "`produced_by` is REQUIRED on this surface, for every call — a sweep and a fully-named \
             series alike. It is the commit-key PREFIX the rows carry, and every key of every \
             matched series must carry it or the whole run is refused. A model composes an \
             identity from context that may be stale; a provenance assertion is the one check that \
             fails on a plausible-but-wrong one. There is no override. Read the prefix off \
             `list_series`, or ask the operator.",
        )?
        .to_string();
    // ⚠ **A producer PATH is refused here, and this surface is the one where it could never have
    // worked.** `--produced-by` has two spellings, and `vike_data::store_kind::resolve_produced_by`
    // — which turns a repo-relative producer path INTO its commit-key prefix — used to have exactly
    // one caller in the tree: the ENGINE's local `data rm` arm.
    // `crates/vike-datahub/src/server.rs`'s `delete_series_verb` called nothing of the kind, and
    // this tool is REMOTE-ONLY. So a path sent from here was asserted as a literal prefix, matched
    // no commit key in any series, and came back as "provenance REFUSED" — which reads as a finding
    // about the operator's data rather than as an argument that was never resolved. The schema
    // above promised the resolution unconditionally until this landed; it now says LITERAL, and
    // this is the refusal that makes the boundary visible where it is crossed.
    //
    // ⚠ **The server half landed on 2026-09-11** (`delete_series_verb` now resolves the spelling
    // through the `vike_datahub_client::proto` re-export of that same function), so this refusal is
    // a COMPATIBILITY guard rather than a stand-in — the protocol is capability-negotiated and
    // carries no string for "this server resolves producer paths", so an agent cannot tell a
    // redeployed datahub from an older one. It also stays because the argument for requiring a
    // LITERAL prefix on THIS surface is independent of the server: an agent composes an identity
    // from context that may be stale, and a prefix it read off `list_series` is evidence from the
    // store while a path is a guess about the tree.
    if produced_by.contains('/') {
        return Err(format!(
            "`produced_by` {produced_by:?} looks like a PRODUCER PATH, and this tool deletes \
             through a datahub — one that has not been redeployed since 2026-09-11 resolves none, \
             and this protocol carries no capability string to tell the two apart, so a path would \
             be asserted as a literal prefix, match no commit key, and report the store as \
             foreign. Pass the commit-key PREFIX literally (e.g. `panel_bars:`); `list_series` \
             shows what the rows carry, which is evidence from the store where a path is a guess \
             about a source tree you cannot see."
        ));
    }
    if symbol.is_some() && group.is_some() {
        return Err(
            "`symbol` and `group` are ALTERNATIVES, not a pair: a GROUPED series has an EMPTY \
             symbol and a per-symbol series has no group. Pass one."
                .to_string(),
        );
    }
    if group.is_some() && interval.is_some() {
        return Err(
            "`interval` does not apply to `group`: a grouped series' leaf has no `interval=` \
             segment at all"
                .to_string(),
        );
    }
    for (field, value) in [
        ("kind", Some(&kind)),
        ("venue", Some(&venue)),
        ("symbol", symbol.as_ref()),
        ("group", group.as_ref()),
        ("interval", interval.as_ref()),
    ] {
        let Some(value) = value else { continue };
        if let Some(c) = value.chars().find(|c| "*?[".contains(*c)) {
            return Err(format!(
                "`{field}` value {value:?} contains the glob character {c:?}. Globs are refused: \
                 OMITTING a dimension already wildcards it"
            ));
        }
    }
    Ok(DeleteIntent { kind, venue, symbol, group, interval, produced_by })
}

/// The MANDATORY preview `delete_series` returns for any call that is not a valid confirm.
///
/// # ⚠ What an agent needs that a human does not
///
/// The plan's lines carry the same facts a terminal shows — the store, each matched series with its
/// rows and its commit keys, the totals, the verdict — and that is not enough here. A human reads
/// "2 series, 8,613 rows" and FEELS the size; a model needs `matched`, `rows` and `bytes` as
/// NUMBERS it can compare against what it expected before it decides to confirm. So the totals are
/// typed fields beside the prose, not only inside it.
///
/// The store LEADS, for the reason it leads at a terminal and one more: a human at a shell knows
/// which box they are on, and an agent has no such context at all.
fn delete_preview(
    intent: &DeleteIntent,
    plan: Option<&vike_datahub_client::proto::RemovalPlan>,
    plan_error: Option<&str>,
    reason: Option<&str>,
    preview_token: &str,
) -> Value {
    json!({
        "will_execute": false,
        "tool": "delete_series",
        "selector": {
            "kind": intent.kind,
            "venue": intent.venue,
            "symbol": intent.symbol,
            "group": intent.group,
            "interval": intent.interval,
        },
        "produced_by": intent.produced_by,
        // The TYPED totals, beside the prose and not only inside it — the thing an agent needs that
        // a human does not. `null` when the store could not be asked, never `0`: a fabricated zero
        // is the one answer a model would read as "nothing to delete, proceed".
        "matched": plan.map(|p| p.matched()),
        "rows": plan.map(|p| p.rows()),
        "bytes": plan.map(|p| p.bytes()),
        "plan": plan.map(|p| p.lines()),
        "provenance_satisfied": plan.map(|p| p.verdict().is_ok()),
        "plan_error": plan_error,
        // ⚠ HONESTLY false, always: this tool touches no vike-tradehub node, so there is no node
        // verdict behind it and never will be. The field is carried because it is the one an agent
        // is taught to read as "the verdict that counts", and its ABSENCE on one write tool would
        // be read as a verdict rather than as its absence. What plays that role here is
        // `provenance_satisfied`, which is the STORE's own answer.
        "verified_by_node": false,
        // A store partition is not a mount — see [`VENUE_CHECK_NONE`].
        "venue_check": VENUE_CHECK_NONE,
        "preview_token": preview_token,
        "reason": reason,
        "note": if plan.is_some() {
            "PREVIEW ONLY — nothing was deleted. `plan` is the SERVER's own dry run against the \
             store it has open: which series matched, what each holds, and the commit keys that \
             wrote them. Read `matched`/`rows`/`bytes` and compare them to what you expected BEFORE \
             confirming — a wrong selector usually shows up as a count you did not predict. To \
             execute, call again with BOTH \"confirm\": true AND this exact \"preview_token\". The \
             token fires once, expires after 60s, and is bound to THIS selector AND this \
             `produced_by` — confirming a different deletion with it is refused. Deletion is \
             IRREVERSIBLE, and a window a venue no longer serves cannot be re-fetched."
        } else {
            "PREVIEW ONLY — nothing was deleted. ⚠ THE STORE WAS NOT ASKED (`plan_error` says why), \
             so there is NO plan: `matched`, `rows` and `bytes` are null rather than zero, and you \
             have been shown nothing to confirm. Do not confirm this. A confirming call would fail \
             at the same connection, so nothing can be deleted through it — fix the connection, \
             take a fresh plan, and read it before you decide."
        },
    })
}

/// The two `node_verdict.checked_by` values, spelled ONCE. [`Server::node_preview`] writes one of
/// them and [`preview_of`] reads it back to decide whether the preview was verified — a question
/// that must be asked of the VERDICT, never of the `Option` wrapping it.
const CHECKED_BY_NODE: &str = "node";
/// The node was reached for, and did not answer — a transport fault, a denied handshake, or the
/// client-side refusal. See [`CHECKED_BY_NODE`].
const CHECKED_BY_NONE: &str = "none";

/// The three `venue_check` dispositions a write reports, spelled ONCE.
/// [`Server::vet_commanded_venue`] writes one of them; it is the one field that says whether the
/// venue in `wire_command` was compared against anything at all.
///
/// ⚠ Reported on BOTH halves of the two-call gate — the preview payload ([`preview_of`]) and the
/// answer to the confirmed call ([`Server::call_tool`]'s write arm). The confirm half is not
/// decoration: `unverified` is an ALLOW, so an accepted write that stated nothing would leave the
/// record of the write that actually happened unable to say whether the gate ran.
///
/// `mounted` — the command names a venue and the node reports mounting it.
const VENUE_CHECK_MOUNTED: &str = "mounted";
/// `none` — the command names NO venue THE NODE COULD MOUNT, so there is nothing to check. The
/// `market_exit` / `mass_cancel` "every engine" shape, and every venue-less verb.
///
/// ⚠ It also covers `delete_series`, whose selector DOES carry a `venue` — a STORE partition, which
/// is not a mount and which the node has no opinion about. Reporting `unverified` there would say a
/// check had been attempted and had failed, which is worse than saying there was none to make.
/// See [`VENUE_CHECK_MOUNTED`].
const VENUE_CHECK_NONE: &str = "none";
/// `unverified` — the command names a venue and the node's mounted set could not be READ (no node,
/// no observe key, or the node is down). The command was NOT refused on an absence, and the
/// preview says so rather than presenting an unmade check as a passed one. See
/// [`VENUE_CHECK_MOUNTED`].
const VENUE_CHECK_UNVERIFIED: &str = "unverified";

/// The venue a write command NAMES, or `None` when it names none.
///
/// ⚠ **Exhaustive on purpose.** A wildcard arm would answer `None` — "nothing to check" — for a
/// future venue-carrying variant, which is the gate silently not covering the verb that needed it.
/// The two strategy-level variants are matched even though this server serves no tool that builds
/// one, so a tool for them inherits [`Server::vet_commanded_venue`] the day it is added rather than
/// needing this function edited as well; both name a mount venue the node must already run, which
/// is the same question.
fn commanded_venue(cmd: &WireCommand) -> Option<&str> {
    match cmd {
        WireCommand::Submit(o) => Some(o.venue.as_str()),
        WireCommand::Flatten { venue, .. } => Some(venue.as_str()),
        // OPTIONAL by declaration, and an omitted one MEANS every engine — see
        // [`Server::vet_commanded_venue`]'s note on why that may not be refused.
        WireCommand::MassCancel { venue, .. } | WireCommand::MarketExit { venue } => {
            venue.as_deref()
        }
        WireCommand::UpdateParams { venue, .. } | WireCommand::MountStrategy { venue, .. } => {
            Some(venue.as_str())
        }
        // Order-scoped by id, account-wide, or not a venue question at all.
        WireCommand::Cancel(_)
        | WireCommand::Modify { .. }
        | WireCommand::SetTradingState(_)
        | WireCommand::UnmountStrategy { .. }
        | WireCommand::SetSetting { .. } => None,
    }
}

/// Does this command's venue pass, and what should the preview report? PURE — the decision half of
/// [`Server::vet_commanded_venue`], which supplies both inputs.
///
/// `venue` is [`commanded_venue`]'s answer; `mounted` is the node's own reported set, `None`
/// meaning it could not be established. `Err` is the refusal TEXT, and it is written to be acted
/// on: it names the offending value, names what the node actually mounts, names the tool that
/// reports it, and states the mechanism — because "not mounted" without the routing consequence
/// reads as a naming quibble rather than as "this would have gone somewhere else".
fn venue_verdict(venue: Option<&str>, mounted: Option<&[String]>) -> Result<&'static str, String> {
    // A command that names NO venue is not a command with a missing one: `market_exit` and
    // `mass_cancel` declare the argument optional and MEAN "every engine" when it is omitted, and
    // `cancel_order` / `modify` / `set_trading_state` have no venue at all. A gate that fired on a
    // valid omission would break the panic button — a worse failure than the one being closed.
    let Some(venue) = venue else {
        return Ok(VENUE_CHECK_NONE);
    };
    let Some(mounted) = mounted else {
        return Ok(VENUE_CHECK_UNVERIFIED);
    };
    // EXACT, never case-folded — see [`Server::vet_commanded_venue`].
    if mounted.iter().any(|m| m == venue) {
        return Ok(VENUE_CHECK_MOUNTED);
    }
    Err(format!(
        "venue {venue:?} is NOT MOUNTED on this node — nothing was previewed, no preview_token was \
         issued, and nothing was sent. This node mounts: {}. Call node_snapshot, read \
         `venues[].venue`, and re-issue the command naming one of them EXACTLY (the comparison is \
         case-sensitive). Nothing further down the line would have caught this: a venue string the \
         node does not mount routes to no engine and falls back to the node's FIRST one, which is \
         not the account you named.",
        mounted.join(", ")
    ))
}

/// What a rejected ENQUEUE says to an agent. [`ControlRejected::Gone`] never reaches here — it is
/// the never-sent case [`Server::offer_command`] handles by reconnecting — so this covers the two
/// that are answers.
///
/// ⚠ It used to be `{e:?}`, which printed the bare enum name. `Busy` at least suggests waiting;
/// **`UnsupportedByNode` suggested nothing at all**, and it is the arm the lifecycle verbs
/// introduced: `mount_strategy` / `unmount_strategy` are refused CLIENT-SIDE against a node whose
/// `Welcome.features` does not advertise the mount capability (an older daemon's serde cannot
/// decode the variant, so sending would produce an opaque decode error at the node instead of a
/// diagnosis here). An agent handed the word alone would read it as a transient fault and retry
/// forever against a node that can never accept it.
fn control_rejection(e: ControlRejected) -> String {
    match e {
        ControlRejected::UnsupportedByNode => "control command not sent: this node does not \
             advertise the capability this verb requires (an older vike-tradehub) — it was REFUSED \
             CLIENT-SIDE and not one byte went on the wire. This is a fact about the node, not \
             about your arguments: retrying against the same node cannot succeed, and the operator \
             has to upgrade it."
            .to_string(),
        ControlRejected::Busy => "control command not sent: the outbound queue to the node is \
             full — the previous command(s) have not drained yet. NOTHING was enqueued for this \
             call; try again in a moment."
            .to_string(),
        // Handled as a never-sent before this is reached; spelled rather than wildcarded so a new
        // variant fails to compile here instead of falling into someone else's sentence.
        ControlRejected::Gone => {
            "control command not sent: the control connection is gone.".to_string()
        }
    }
}

/// What a per-call node READ failure says to an agent — the tool-shaped twin of
/// `crate::cmd::trade_status`'s `failure_lines`, which writes the same three diagnoses for a
/// human at a terminal.
///
/// PURE, and it exists because two failures out of the three are CONFIGURATION facts about a
/// reachable box rather than transport trouble, and an agent that cannot tell them apart retries
/// the one thing that can never succeed:
///
/// * `Unsupported` — the client refused before sending, because the node's `Welcome.features` does
///   not advertise this verb's capability. Nothing went on the wire; the fix is on the NODE and
///   the message says so, because "unsupported" alone reads as a bug in the request.
/// * `PermissionDenied` — the handshake itself was refused: the observe key presented does not
///   verify against that node's. Points at the key, not the verb.
/// * anything else — an honest transport-shaped report naming the address, which is the one an
///   agent may sensibly try again.
///
/// ⚠ The client's own sentence is KEPT in every arm rather than replaced: it names the capability
/// string, which is the thing an operator greps for.
fn node_read_failure(tool: &str, addr: &str, err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::Unsupported => format!(
            "{tool}: {err}. This is a fact about the NODE, not about your request — the operator \
             has to upgrade the vike-tradehub at {addr} to a build that serves this verb. Calling \
             it again against the same node cannot succeed."
        ),
        io::ErrorKind::PermissionDenied => format!(
            "{tool}: the node at {addr} refused the observe handshake: {err}. The presented \
             {observe} does not match that node's — `vike-cli secrets path` prints the store this \
             side read it from.",
            observe = nodekeys::OBSERVE_KEY_ENV
        ),
        _ => format!("{tool}: cannot query the node at {addr}: {err}"),
    }
}

/// The NODE-LIFECYCLE subset of [`WRITE_TOOLS`]: the writes that change what the node RUNS or how
/// it is CONFIGURED rather than what is in its book.
///
/// ⚠ It no longer says where these three are BUILT — [`crate::cmd::verbs`] builds all ten, like
/// every other write, since the `trade` REPL grew its own spelling of them. What is left keyed on
/// this array is the one thing that really is lifecycle-specific: [`instructions`] scopes its
/// [`INSTRUCTIONS_LIFECYCLE`] clause on it, so a fourth one joins that clause by construction.
///
/// It is a SUBSET and never a second roster: `the_lifecycle_tools_are_a_subset_of_the_write_roster`
/// holds every name here to [`WRITE_TOOLS`], because a lifecycle tool that fell out of that array
/// would lose the mandatory preview gate while keeping everything that makes it look gated.
const LIFECYCLE_TOOLS: [&str; 3] = ["mount_strategy", "unmount_strategy", "set_setting"];

/// The TYPED-CONFIRM gate: a `policy.toml` write that arrives with no `policy_confirm` is refused
/// HERE, before a preview is rendered or a token is minted. PURE — the input is the built command,
/// so this needs no node and no `self`.
///
/// # What this side checks, and what it deliberately leaves to the node
///
/// It checks PRESENCE and nothing else. `vike_tradehub::server`'s `apply_set_setting` is the
/// authority for the contract and enforces both halves — a missing confirm and a MISMATCHED one
/// get distinct refusals there, each naming the expected spelling — and this gate does not
/// duplicate the equality compare, for a reason that is not "the node already does it":
///
/// * a compare here would be this process deciding whether a string it could have written itself
///   matches a string it holds. It can prove nothing about the property the contract is actually
///   for, which is that a HUMAN retyped the key; and
/// * the code that performs the compare is one edit away from the code that could satisfy it. The
///   fence is stronger when this side has no equality logic at all to be "helpfully" inverted.
///
/// So the split is: this side refuses the shape that could never be a retyping (nothing was typed),
/// and the node judges what was.
///
/// # Why it is a refusal and not a preview
///
/// The alternative — preview it and let the confirming call fail — reads worse than it sounds. The
/// node's dry-run (`Request::Preview`) vets the notional cap and nothing else, so a policy write
/// with no confirm PREVIEWS AS ACCEPTED and is refused only when it is sent. An agent would be
/// shown an approving verdict for a command that cannot land, which is the exact failure
/// `verified_by_node` exists to prevent one layer up.
fn typed_confirm_verdict(cmd: &WireCommand) -> Result<(), String> {
    let WireCommand::SetSetting { file, key, confirm, .. } = cmd else {
        return Ok(());
    };
    // The bare stem is accepted on the wire, so match it the way the node's `SettingsFile::parse`
    // does rather than on the full filename alone.
    let is_policy = file.trim().trim_end_matches(".toml") == "policy";
    if is_policy && confirm.is_none() {
        return Err(format!(
            "policy.toml holds this node's RISK CEILINGS, so a policy write needs the typed \
             confirm: re-send with `policy_confirm` set to the exact key `{key}`. ⚠ Ask the \
             OPERATOR to retype it — do not copy it out of your own `key` argument. Nothing was \
             previewed, no preview_token was issued, and nothing was sent."
        ));
    }
    Ok(())
}

/// The mandatory-preview payload for a write tool: the resolved command, the SHARED client-side
/// guardrail check ([`crate::cmd::verbs::guardrail_check`] — advisory; the node's server-side gate
/// is the enforcing one), the rationale that will be recorded, and the confirm hint. PURE.
///
/// `reason` is ECHOED here on purpose: it is the one part of the request the agent cannot otherwise
/// see the effect of (it goes to the node's audit trail, not to the order), so the preview shows it
/// alongside the command it will be filed against. It is echoed as SENT — the node applies its own
/// sanitization (`vike_tradehub::audit::sanitize_reason`) before recording.
///
/// `venue_check` is [`Server::vet_commanded_venue`]'s verdict, carried here rather than recomputed:
/// a preview that reached this function was NOT refused, so the only thing left to disclose is
/// whether the venue was compared against the node's mounted set at all. It is reported even in the
/// `mounted` case, because "checked and fine" and "not checked" must not look the same to an agent
/// — the same rule `verified_by_node` is written to above.
fn preview_of(
    name: &str,
    cmd: &WireCommand,
    reason: Option<&str>,
    caps: verbs::GuardrailCaps,
    preview_token: &str,
    node: Option<Value>,
    venue_check: &'static str,
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
        "venue_check": venue_check,
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

/// The TYPED confirm a `policy.toml` write carries. A SECOND confirmation with nothing to do with
/// [`confirm_property`]'s preview gate, and it needed its own NAME for exactly that reason: this
/// server already owns the argument called `confirm`, so the wire field
/// `WireCommand::SetSetting::confirm` could not keep its own spelling here without one of the two
/// meanings silently winning.
///
/// ⚠ **This server never fills it in, and it is the party best placed to.** It holds `key` in
/// hand — copying it across would satisfy the node's compare on every call and cost nothing, which
/// is precisely why it must not: `vike_tradehub::server`'s `apply_set_setting` states the division
/// outright ("the client's job is to make the operator TYPE it (never pre-fill); this arm's job is
/// to refuse anything else"), and a value this server synthesised would prove that a program can
/// retype a string. So the schema ASKS for it, [`typed_confirm_verdict`] refuses a policy write
/// that arrives without one before a token is minted, and the EQUALITY compare stays the node's —
/// see that function for why this side deliberately does not perform it.
fn policy_confirm_property() -> Value {
    json!({
        "type": "string",
        "description": "REQUIRED for a policy.toml write; ignored for the other three files. The exact dotted `key`, RETYPED BY THE OPERATOR — policy holds the risk ceilings, and the point of this field is that a human typed the name of the one being moved. Do NOT copy `key` into it: ask the operator, and pass back what they gave you. The node compares it to `key` and refuses a mismatch."
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
/// ⚠ The last three are NODE-LIFECYCLE verbs ([`LIFECYCLE_TOOLS`]) rather than orders, and they
/// are on this roster because what a roster membership BUYS is the mandatory preview gate, the
/// `destructiveHint`, the `read-only` withholding and the transcript's write classification —
/// every one of which a command that re-mounts a strategy or moves a risk ceiling needs at least
/// as much as an order does.
///
/// ⚠ This paragraph said the three were verbs "the `trade` REPL has no spelling for at all", which
/// was true for about a week: two agents added them to the two surfaces in parallel, and the note
/// went stale the moment the second one landed. It is again exactly the REPL's write-verb set, and
/// all ten resolve through [`crate::cmd::verbs`].
/// ⚠ The LAST member is not a node command at all, and its membership is the whole of its gating.
/// `delete_series` removes stored history through a datahub; it resolves through NO
/// `crate::cmd::verbs` spelling and must not grow one. What being on this roster BUYS it is exactly
/// what it needs: the mandatory preview, the intent-bound token, the `destructiveHint`, the
/// `read-only` withholding and the transcript's write classification — four edges from one line.
/// Removing it from here would silently drop all four in one edit, which is the failure
/// `the_data_deleter_keeps_every_guard` exists for.
pub const WRITE_TOOLS: [&str; 11] = [
    "submit_order",
    "cancel_order",
    "modify",
    "flatten",
    "market_exit",
    "set_trading_state",
    "mass_cancel",
    "mount_strategy",
    "unmount_strategy",
    "set_setting",
    "delete_series",
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
/// would be a write with the whole gate routed around it. That is why `read-only` withholds neither
/// of them — and why `offline` withholds BOTH, since both reach a tool that opens a socket.
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

/// WHICH TOOL each resource is a second way in to — the mapping that lets one scope gate cover both
/// surfaces.
///
/// ⚠ It is a table rather than a field on the spec because it states a fact about the
/// IMPLEMENTATION ([`Server::read_resource`]'s arms), not about the advertisement, and the two must
/// be compared rather than assumed equal: `every_resource_is_a_way_in_to_a_named_tool` walks
/// [`resources_spec`] against this in both directions, so a third resource added with no row here
/// reddens rather than quietly escaping the profile.
const RESOURCE_TOOLS: [(&str, &str); 2] =
    [("vike://node/snapshot", "node_snapshot"), ("vike://backtest/last", "run_backtest")];

/// The `resources/list` payload for one [`ToolAccess`] — [`resources_spec`] minus every resource
/// whose tool this session withholds.
fn resources_spec_for(access: &ToolAccess) -> Value {
    let spec = resources_spec();
    let kept: Vec<Value> = spec
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|r| {
            let uri = r["uri"].as_str().unwrap_or_default();
            RESOURCE_TOOLS
                .iter()
                .find(|(u, _)| *u == uri)
                .is_none_or(|(_, tool)| access.admits(tool))
        })
        .cloned()
        .collect();
    Value::Array(kept)
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
fn two_call_gate(access: &ToolAccess) -> String {
    // ⚠ The roster is the ADMITTED write set, not [`WRITE_TOOLS`] whole. Under a scoped profile the
    // sheet must describe the session the agent is actually in: teaching the two-call gate for tools
    // this server does not serve is teaching a procedure whose first call is a refusal. When NONE is
    // served there is no gate to teach at all, and [`scope_banner`] — which every prompt carries at
    // the TOP, where it is read before the steps rather than after them — says so instead.
    let served: Vec<&str> = WRITE_TOOLS.iter().copied().filter(|t| access.admits(t)).collect();
    if served.is_empty() {
        return String::new();
    }
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
        roster = served.join(", "),
        secs = PREVIEW_WINDOW.as_secs(),
    )
}

/// The line every prompt opens with when this session serves NO order-write tool — `None` under a
/// profile that serves at least one.
///
/// ⚠ It sits at the TOP of the sheet, before the steps, and that placement is the point. The steps
/// of `triage_a_stuck_order` legitimately name `modify` and `cancel_order` — they are the right
/// answer to a stuck order — so a sheet that only mentioned the scope at the END would have an
/// agent plan two calls it cannot make before reading that it cannot make them. It also states that
/// the agent cannot widen this itself, because the failure mode of a refusal without that clause is
/// an agent that spends its next turns hunting for the switch.
fn scope_banner(access: &ToolAccess) -> Option<String> {
    if WRITE_TOOLS.iter().any(|t| access.admits(t)) {
        return None;
    }
    Some(format!(
        "⚠ THIS SESSION SERVES NO ORDER-WRITE TOOL. The server is running under the `{profile}` \
         tool profile, so every write tool named below is absent from `tools/list` and is refused \
         if called: nothing you do here can place, modify or cancel an order. Only the OPERATOR can \
         change that, by restarting the server with `--profile full` in the MCP client's launch \
         command — so where a step below says to act, READ and REPORT instead, and say plainly what \
         a human would have to run.",
        profile = access.profile_name(),
    ))
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
fn render_prompt(name: &str, args: &Value, access: &ToolAccess) -> Result<String, String> {
    let arg =
        |key: &str| args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    // Rendered once for the arms that splice it — the roster, the window and now the SCOPE it names
    // are all DERIVED (see [`two_call_gate`]), which is why it is a value here rather than a `const`
    // in scope.
    let gate = two_call_gate(access);
    let body = match name {
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
8. Read `run_walk_forward`'s two forms as two different claims. With `n_splits` alone it \
                 trades YOUR parameters in every window — evidence that those settings held \
                 up, and nothing about a procedure. With `search = \"sweep\"` in \
                 `[walkforward]` each window re-fits on its own training half and trades only \
                 its winner, which is the claim people usually mean by \"walk-forward\". Run \
                 BOTH and report the pair: the no-search control is the only thing that says \
                 whether the fitting bought anything, and on our own data it has come back \
                 saying it bought nothing.

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
    }?;
    // The scope banner goes FIRST, on every sheet — see [`scope_banner`] for why the position is
    // load-bearing rather than cosmetic.
    Ok(match scope_banner(access) {
        Some(banner) => format!("{banner}\n\n{body}"),
        None => body,
    })
}

/// Every tool this server IMPLEMENTS — each one's schema + risk annotations.
///
/// ⚠ This is the FULL roster, not the answer to `tools/list`: a session serves
/// [`ToolAccess::advertised`], which is this filtered by the profile. It stays whole deliberately —
/// `server.json`'s registry listing and the `skills/` package describe the SERVER, not one launch of
/// it, and both gates read this function.
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
            // ⚠ It reads as an INSPECTION tool for somebody else's script, and that is how the
            // first agent-eval model run read it: having authored the strategy itself, the agent
            // answered "which parameters can I tune" from its own memory of what it had just
            // written and never called this. It happened to be right and had no way to know it —
            // `validate_strategy` answers `{ok: true}` and says nothing about params, and this tool
            // runs the TOP LEVEL only, so the one mistake worth catching (a `param()` inside a
            // hook, which nothing reports) is invisible to the author too. So the text now says what
            // the tool is FOR and what it cannot see. `crates/vike-agent-eval/src/cases.rs`'s
            // `WRITE_A_RHAI_STRATEGY` is the measurement.
            //
            // ⚠ That rewrite OVERSHOT on one clause: it said a `param()` inside `on_bar()` was
            // "undrivable by a sweep", which made the tool read as a correctness gate over what a
            // grid can reach. It is false, and both halves were read back off the code before this
            // was reworded. `crates/vike-script/src/engine.rs`'s `param` registration reads
            // `overrides` on EVERY call rather than only during the one-time top-level run, and
            // `crates/vike-backtest/src/harness/registry.rs`'s `rhai_overrides` forwards every
            // numeric param key except `src` with no comparison against the declared set — so a
            // grid key naming a hook-buried knob reaches it and drives it. The real limitation is
            // narrower and duller: discovery never REPORTS such a knob, so nothing tells the author
            // (or whoever writes the grid) that it is there or what its default is. Claiming more
            // than that teaches an agent to conclude a working grid is inert.
            "description": "Read back the tunable param(name, default) knobs a Rhai strategy declares — the exact keys a profile's [strategy.params] or a [sweep] grid may override, in declaration order. Call it on a script you WROTE as well as one you were handed: it runs the script's TOP LEVEL only, so a param() inside on_bar() is NOT REPORTED here (a sweep can still drive such a knob by name — but nothing tells you it is there), and compiling clean says nothing about it. Offline — no server.",
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
            "description": "Run a backtest on the remote backtest daemon (`vike-backend backtest --addr`, dialled at --backtest-addr) from a profile TOML, optionally injecting a Rhai script. Returns the BacktestReport JSON.",
            "inputSchema": {
                "type": "object",
                "properties": { "profile": { "type": "string" }, "script": { "type": "string" } },
                "required": ["profile"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "run_sweep",
            "description": "Run a parameter-grid sweep on the remote backtest daemon (--backtest-addr) from a profile TOML with a [sweep] table (each key overrides strategy.params.<key> across a value grid), optionally injecting a Rhai script. Returns the server-ranked SweepReport JSON (one row per grid point, each with its BacktestReport, best first).",
            "inputSchema": {
                "type": "object",
                "properties": { "profile": { "type": "string", "description": "a backtest profile TOML with [data]/[strategy]/[sweep] (+ optional [engine])" }, "script": { "type": "string", "description": "optional Rhai source injected as [strategy.params].src" }, "rank_by": { "type": "string", "description": "sharpe (default) | return | max_dd | equity — applied server-side" } },
                "required": ["profile"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "run_walk_forward",
            "description": "Run an out-of-sample walk-forward on the remote backtest daemon (--backtest-addr) from a profile TOML with a [walkforward] table, optionally injecting a Rhai script. DEFAULT (n_splits alone) is the FIXED-parameter stability walk: every window trades the profile's own [strategy.params] and chooses nothing — do NOT report such a run as an optimization. search = \"sweep\" in [walkforward] runs the other protocol: each window re-scores the [sweep] grid on its OWN training half and trades only that window's winner (mode = anchored|rolling, rank_by = sharpe|return|max_dd|equity); it needs a [sweep] table and is refused without one. Returns the stitched WalkForwardReport JSON (OOS windows + oos_sharpe + wf_consistency) — only a window that searched carries chosen_params, which is how you tell the two runs apart. Run both on one profile: the no-search control is the only evidence that searching bought anything.",
            "inputSchema": {
                "type": "object",
                "properties": { "profile": { "type": "string", "description": "a backtest profile TOML with [data]/[strategy]/[walkforward] (+ optional [engine]; a [sweep] table too when [walkforward].search = \"sweep\")" }, "script": { "type": "string", "description": "optional Rhai source injected as [strategy.params].src" } },
                "required": ["profile"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "list_strategies",
            "description": "List the compiled native backtest strategies the remote backtest daemon offers (the names a profile's strategy.name can resolve). Requires a reachable backtest daemon (--backtest-addr).",
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
            "name": "strategy_status",
            // ⚠ The last clause is a DISCLOSURE, not padding. `WireMountRow` carries
            // strategy/params/live and NOTHING ELSE — no mount id, no venue/symbol/interval — and
            // an agent that assumed otherwise would plan an `unmount_strategy` off a field that is
            // not in the payload, then invent one. Saying where the id really comes from is the
            // whole difference between a read it can act on and a read it will guess past.
            "description": "Ask the running vike-tradehub node WHAT IT IS RUNNING: which daemon answered (name, live/paper, build), the daemon's resolved effective-params line, and one row per mounted strategy (strategy name, that mount's params, and whether THAT MOUNT trades live — a per-venue fact, which may disagree with the daemon's own live flag). Read-only, over the OBSERVE scope: it can neither place nor change anything. Requires --node + VIKE_TRADEHUB_OBSERVE_KEY and a node that advertises the strategy verbs; an older node is refused CLIENT-SIDE with nothing sent. ⚠ It does NOT report mount IDs — see unmount_strategy for where one comes from.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        },
        {
            "name": "settings_show",
            // ⚠ NO subject word from `the_mcp_surface_advertises_no_credential_writer`'s SUBJECT
            // list may appear here, and the temptation is real: the natural sentence for the
            // redaction clause reaches for the word this surface may not pair with an act word.
            // "sensitive-looking" carries the same fact and trips nothing — and the redaction is
            // the NODE's anyway (`WireSettingsRow`'s doc: rows arrive already redacted, on
            // construction, so no serializer on this side could leak one).
            "description": "Read the running vike-tradehub node's EFFECTIVE settings: the settings directory it resolved at boot, then one row per typed key — which file it belongs to, its full dotted key, the effective value, the LAYER that set it (a default, a file, an environment variable), and what actually READS it. Sensitive-looking values arrive already redacted by the node. The FILES half only: the box's environment registry stays a local disclosure. Read-only, over the OBSERVE scope. Requires --node + VIKE_TRADEHUB_OBSERVE_KEY and a node that advertises the settings-show capability; an older node is refused CLIENT-SIDE with nothing sent. `rows[].key` is the exact spelling set_setting takes.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
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
            // ⚠ The second sentence is here because an agent REACHED FOR THIS TOOL to "switch
            // binance to live". The first agent-eval model run refused correctly — no write, no key
            // echoed — and then told the operator the remedy was "the node's config/env and a
            // restart", which cannot work: no environment variable arms a venue, and the operator
            // who follows that edits something nothing reads and stays on paper with no error. This
            // is the only tool on the roster whose name reads like "go live", and it now says what
            // it is not and where the decision actually lives.
            //
            // ⚠ It names the POLICY half and stops there, deliberately. The credential half is
            // fenced by `docs/decisions/0036…` and by
            // `the_mcp_surface_advertises_no_credential_writer` below: this description may not
            // pair a credential word with a write word, and it opens with "Set". The human-facing
            // verb lives in the `arm_a_venue` prompt and the `arm-a-venue` skill, which the scan
            // deliberately does not walk and which execute nothing.
            "description": "Set the account trading state / kill switch on the node — active | reducing | halted. WITHOUT confirm:true this only PREVIEWS. ⚠ It is NOT a venue arming control and cannot move a venue off the paper simulator: which venues may go live is `policy.venues.<venue>` under [venues] in <project>/settings/policy.toml, a file a human edits on the box and that nothing on this server can change.",
            "inputSchema": {
                "type": "object",
                "properties": { "state": { "type": "string", "description": "active | reducing | halted" }, "reason": reason_property(), "confirm": confirm_property(), "preview_token": preview_token_property() },
                "required": ["state"]
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "mount_strategy",
            // ⚠ The `rhai` clause says "PATH ON THE NODE" twice because the obvious agent mistake
            // is to paste the script it just wrote with `validate_strategy` into this argument.
            // That string is then a filename the daemon cannot open, and the refusal arrives from
            // the node rather than from the schema — a whole round trip to learn that this surface
            // has no way to put a file on that box at all.
            "description": "Add ONE strategy mount to the running vike-tradehub node's core, LIVE and without a restart. WITHOUT confirm:true this only PREVIEWS (executes nothing). The source is the profile [strategy] vocabulary: EXACTLY ONE of `name` (a registry strategy the node compiles in) or `rhai` (a script PATH ON THE NODE's filesystem — not script source; this server cannot put a file on that box). `venue` must name an engine the node already runs — read it from node_snapshot's venues[].venue. The node validates at its edge with the same refusals a profile load applies; a refusal the core itself raises later (a duplicate mount id, an unknown venue) surfaces in the node's recent events rather than here. Requires a node that advertises the mount verbs; an older one is refused CLIENT-SIDE with nothing sent.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "venue": { "type": "string", "description": "an engine the node ALREADY runs — node_snapshot's venues[].venue, exactly (the comparison is case-sensitive)" },
                    "symbol": { "type": "string", "description": "the mount's own symbol" },
                    "interval": { "type": "string", "description": "the mount's bar-series interval, e.g. `1m`" },
                    "controller_id": { "type": "string", "description": "optional explicit mount id; omitting it derives `{venue}__{symbol}__{interval}`. Whatever this resolves to is what unmount_strategy will need" },
                    "name": { "type": "string", "description": "a REGISTRY strategy name — exactly one of `name` / `rhai`" },
                    "rhai": { "type": "string", "description": "a Rhai script PATH on the NODE's filesystem — exactly one of `name` / `rhai`. NOT script source" },
                    "params": { "type": "object", "description": "the [strategy.params] table; omitted means an empty table. Each strategy reads its own knobs, so this wire never re-declares them" },
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                },
                "required": ["venue", "symbol", "interval"]
            },
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        },
        {
            "name": "unmount_strategy",
            // ⚠ Two clauses an agent cannot get from anywhere else, and both were read off the
            // wire variant's own doc. The positions one is the safety half: "unmount" reads as
            // "stand this down", and what it really leaves behind is an OPEN POSITION with nothing
            // managing it. The mount-id one is the usability half — `strategy_status` reports no
            // id, so an agent told to "read it from the status" would go looking for a field that
            // is not in the payload.
            "description": "Remove ONE strategy mount from the running vike-tradehub node's core by MOUNT ID. WITHOUT confirm:true this only PREVIEWS. The node CANCELS that mount's attributed live orders before removing it and saves its durable state — but POSITIONS ARE NOT FLATTENED: whatever the strategy left open stays open with nothing managing it, and `flatten` is the verb for that. ⚠ The id is the explicit `controller_id` the mount was created with, or the derived `{venue}__{symbol}__{interval}` — strategy_status does NOT report mount ids, so an id you did not mint yourself has to come from the operator or from that derivation. An unknown id surfaces in the node's recent events rather than as an error here. Requires a node that advertises the mount verbs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "controller_id": { "type": "string", "description": "the mount id to remove — the explicit one given at mount time, or `{venue}__{symbol}__{interval}`" },
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                },
                "required": ["controller_id"]
            },
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        },
        {
            "name": "set_setting",
            // ⚠ Like `set_trading_state` above, this description may name no credential: the
            // SUBJECT×ACT scan in `the_mcp_surface_advertises_no_credential_writer` would refuse
            // the whole surface for pairing one with the word `set` in this tool's own NAME. That
            // is not a wording problem to route around — the fact an agent needs (that nothing
            // here reaches a credential) is stated once, for the whole server, by
            // [`INSTRUCTIONS_NO_CREDENTIAL`], which is the right altitude for it anyway.
            //
            // ⚠ A MEASURED CONSEQUENCE of the argument being called `key`: the agent transcript
            // records its value as redacted. `crate::cmd::mcp_trace`'s key rule is
            // `vike_config::is_secret_key`, the workspace's ONE authority on a credential-shaped
            // NAME, and that function matches the bare word `KEY` — so `key`, `setting_key` and
            // every other spelling of this concept is caught. It is not fixable from here and must
            // not be: a second redaction table is precisely what that module refuses to spell.
            // What survives in the record is the tool, the `file`, the `value` and the verdict;
            // what is lost is WHICH key a REFUSED attempt named. The accepted case is unaffected —
            // the node's own audit trail records the write with its old and new values, which is
            // where an executed change is recorded anyway.
            // `the_transcript_redacts_the_settings_key_and_keeps_the_rest` pins both halves, so
            // this stays a known bound rather than a surprise.
            "description": "Write ONE key in ONE of the running node's four settings files (policy.toml | config.toml | preferences.toml | flags.toml), comment-preserving and validated by the NODE's own loader before a byte lands on disk — so a write can never produce a file the next boot refuses. WITHOUT confirm:true this only PREVIEWS. The answer's `restart_required` says which happened: false = the node applied the value LIVE, true = the write is on disk and is what the NEXT boot loads (every policy.toml key is always true — policy is never hot). ⚠ policy.toml holds this node's RISK CEILINGS, and such a write additionally needs `policy_confirm` — the operator's own retyping of the exact dotted key. Read settings_show first: `rows[].key` is the exact spelling this takes, and `rows[].origin` says whether a file is even what sets it today. Requires a node that advertises the settings-write capability; an older one is refused CLIENT-SIDE with nothing sent.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "policy.toml | config.toml | preferences.toml | flags.toml (the bare stem without `.toml` is accepted too)" },
                    "key": { "type": "string", "description": "the FULL dotted key exactly as settings_show renders it (`config.tradehub_addr`, `policy.max_notional_per_order`); its first segment must name the same file `file` does" },
                    "value": { "type": "string", "description": "the new value as TEXT — parsed as a TOML value (`250`, `true`, `[\"a\"]`) when it is one, else written as a string, and then the whole would-be file goes through the node's loader" },
                    "policy_confirm": policy_confirm_property(),
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                },
                "required": ["file", "key", "value"]
            },
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        },
        {
            "name": "delete_series",
            // ⚠ The description carries the ASYMMETRY with the CLI, because nothing else can: an
            // agent reading `vike-cli data rm`'s help would learn that `--produced-by` is optional
            // for a fully-named series, and it is NOT optional here. Saying so in the tool's own
            // text is what stops a model treating its own refusal as a bug in the schema.
            "description": "DELETE stored data series from the datahub's history store, IRREVERSIBLY. Selects on the four series dimensions: `kind` and `venue` are required, and an OMITTED `symbol`/`group`/`interval` is a wildcard over that dimension (there are no globs — omission is the only wildcard). ⚠ `produced_by` is REQUIRED on EVERY call here, unlike the `vike-cli data rm` command, where it is optional for a fully-named series: every commit key of every matched series must carry that prefix, and ONE foreign key refuses the whole run and deletes nothing. That is deliberate — you compose an identity from context that may be stale, and a provenance assertion is the one check that fails on a plausible-but-wrong one. WITHOUT confirm:true AND a matching preview_token this only PLANS: it returns which series matched, what each holds, and the commit keys that wrote them, with `matched`/`rows`/`bytes` as numbers to compare against what you expected. Read `list_series` first if you are unsure what the store holds. Deletion cannot be undone, and a window a venue no longer serves cannot be re-fetched at all. Requires a datahub that holds node keys — a key-less one serves no delete verb.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "description": "the EXACT stored kind (bar | quote | trade | book | depth | properties | …), never a substring" },
                    "venue": { "type": "string", "description": "the EXACT venue slug, never a substring. ⚠ It names an EXCHANGE, never a data SOURCE — rows fetched from a metrics vendor land under the exchange they describe, which is why `produced_by` is the only thing that tells two producers of one series apart" },
                    "symbol": { "type": "string", "description": "the EXACT symbol of a PER-SYMBOL series. Omit to wildcard the dimension. Alternative to `group`, never a pair" },
                    "group": { "type": "string", "description": "the EXACT group of a GROUPED series (which holds many symbols in one part and has NO symbol of its own). Alternative to `symbol`" },
                    "interval": { "type": "string", "description": "the EXACT bar interval (`1h`). Omit to wildcard; refused together with `group`, whose leaf has no interval segment" },
                    "produced_by": { "type": "string", "description": "REQUIRED. The commit-key PREFIX every key of every matched series must carry (`panel_bars:`, `pmxt:quote:`). ⚠ A LITERAL prefix, never a producer PATH: this tool deletes through a datahub, and a path is refused here before anything is dialled. Read the prefix off `list_series` — that is evidence from the store, where a path is a guess about a source tree you cannot see; and a datahub that has not been redeployed since 2026-09-11 resolves no path at all, so one sent there would be asserted literally, match no key, and report the store as foreign when in fact the argument was never resolved. One key that does not carry it refuses the whole run. There is no override and no --force." },
                    "reason": reason_property(),
                    "confirm": confirm_property(), "preview_token": preview_token_property()
                },
                "required": ["kind", "venue", "produced_by"]
            },
            // `idempotentHint: true` is HONEST rather than reassuring: the underlying delete is
            // idempotent, so a retry after a dropped reply does not double-delete. `openWorldHint`
            // is TRUE because this reaches a datahub over a socket — which is also what withholds
            // it from the `offline` ring.
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": true }
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

    /// ⚠ **COMPOSED, never spelled.** `crates/vike-config/tests/policy_is_consumed.rs` scans every
    /// non-comment line under `src/` for a section-qualified policy key and reads a hit as evidence
    /// that the file READS that setting — and unlike its sibling `settings_are_consumed.rs` it does
    /// NOT truncate at `#[cfg(test)]`, so a fixture spelling one turns `main` red. It did: #1685
    /// lifted these fixtures here from `mcp.rs` and `Policy::max_leverage` is `Consumed::No`.
    /// Composing it is the same trick `crates/vike-model/src/credential_keys.rs` uses to keep its
    /// near-miss fixtures out of the settings-registry literal harvest.
    const POLICY_KEY_FIXTURE: &str = concat!("policy.", "max_leverage");

    use super::*;

    // The EXECUTION gate on the shipped templates (see `every_shipped_template_reaches_the_broker`):
    // the real `RhaiStrategy` mounted over vike-model's shared `MockBroker` (its `test-support`
    // feature, already a dev-dep of this crate for `tests/init_cli.rs`) and driven over real bars.
    use std::sync::{Arc, Mutex};

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

    /// Arguments broad enough for EVERY write tool at once — the twin of
    /// `crates/vike-cli/tests/mcp_transcript.rs`'s helper of the same name, and it has to stay one:
    /// the tools take disjoint argument sets and each ignores what it does not name, so the union
    /// is what lets a loop walk [`WRITE_TOOLS`] with no per-tool table to fall out of step with it.
    ///
    /// ⚠ **A NEW write tool whose required arguments are not in here fails the loops rather than
    /// skipping them**, which is the behaviour to keep: the failure names the tool, and the fix is
    /// one line. (`file` deliberately names `config.toml` rather than the policy file — the policy
    /// typed-confirm has its own tests, and putting it in the union would make every write-roster
    /// loop depend on that gate.)
    fn every_write_tools_arguments() -> Value {
        json!({
            "venue": "sim",
            "symbol": "BTCUSDT",
            "side": 1,
            "qty": 1.0,
            "client_order_id": "c1",
            "new_qty": 2.0,
            "state": "halted",
            "interval": "1m",
            "controller_id": "sim__BTCUSDT__1m",
            "name": "spread_maker",
            // …and `delete_series`' own required set. `kind`/`venue` are its SELECTOR (exact,
            // never a substring) and `produced_by` is REQUIRED on this surface for every call —
            // see that tool's doc for why it is unconditional here and conditional on the CLI.
            "kind": "bar",
            "produced_by": "klines:",
            "file": "config.toml",
            "key": "config.tradehub_addr",
            "value": "127.0.0.1:7879"
        })
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

    // ---- tool scoping (`--profile`, `--deny-tool`) -------------------------------------------

    /// EXACTLY what `--profile read-only` serves, spelled out.
    ///
    /// ⚠ A positive list rather than a count, and rather than a filter re-derived here: a count
    /// passes when one tool is swapped for another, and a re-derived filter is the implementation
    /// agreeing with itself. A NEW tool breaks this test, which is correct — it is the
    /// "adding a venue reddens every table" property applied to the tool roster: whoever adds a
    /// tool decides which rings serve it, in a review, rather than inheriting a ring by accident.
    const READ_ONLY_TOOLS: [&str; 12] = [
        "validate_strategy",
        "discover_params",
        "list_templates",
        "list_indicators",
        "run_backtest",
        "run_sweep",
        "run_walk_forward",
        "list_strategies",
        "list_series",
        "node_snapshot",
        // The two per-call node reads. They belong here on the SAME argument every row above them
        // does: `read-only` is `!is_write_tool`, and neither of these can change a byte on the node
        // — `strategy_status` and `settings_show` authenticate under the OBSERVE scope, which the
        // node will not accept a command on at all.
        "strategy_status",
        "settings_show",
    ];

    /// …and EXACTLY what `--profile offline` serves: the four that open no socket.
    const OFFLINE_TOOLS: [&str; 4] =
        ["validate_strategy", "discover_params", "list_templates", "list_indicators"];

    fn advertised_names(access: &ToolAccess) -> Vec<String> {
        access
            .advertised()
            .as_array()
            .expect("tools/list is an array")
            .iter()
            .map(|t| t["name"].as_str().expect("every tool is named").to_string())
            .collect()
    }

    fn server_under(profile: Profile) -> Server {
        Server { access: ToolAccess::new(profile, Vec::new()), ..test_server() }
    }

    #[test]
    fn the_default_profile_serves_the_roster_that_shipped_before_the_flag() {
        // Held equal to `tools_spec` itself rather than to a hand-written list of seventeen names:
        // "identical to today's roster" is the actual claim, and a third copy of that roster (after
        // `server.json`'s) is a third thing to keep in step.
        assert_eq!(
            ToolAccess::full().advertised(),
            tools_spec(),
            "an absent --profile must serve exactly what this server implements"
        );
        for name in served_tool_names() {
            assert!(ToolAccess::full().admits(&name), "{name} must be routed under the default");
        }
    }

    #[test]
    fn the_read_only_profile_serves_exactly_the_reads() {
        let access = ToolAccess::new(Profile::ReadOnly, Vec::new());
        assert_eq!(advertised_names(&access), READ_ONLY_TOOLS);
        for tool in WRITE_TOOLS {
            assert!(!access.admits(tool), "{tool} is a write and must be withheld");
        }
    }

    #[test]
    fn the_offline_profile_serves_exactly_the_tools_that_open_no_socket() {
        let access = ToolAccess::new(Profile::Offline, Vec::new());
        assert_eq!(advertised_names(&access), OFFLINE_TOOLS);
        // The ring is the tool's OWN declaration read back, so this is also a check that every
        // offline tool really does declare it.
        let spec = tools_spec();
        for tool in spec.as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let offline = tool["annotations"]["openWorldHint"] == json!(false);
            assert_eq!(offline, OFFLINE_TOOLS.contains(&name), "{name}: openWorldHint disagrees");
        }
    }

    #[test]
    fn the_rings_nest() {
        let (full, read_only, offline) = (
            ToolAccess::full(),
            ToolAccess::new(Profile::ReadOnly, Vec::new()),
            ToolAccess::new(Profile::Offline, Vec::new()),
        );
        for name in served_tool_names() {
            if offline.admits(&name) {
                assert!(read_only.admits(&name), "{name}: offline ⊄ read-only");
            }
            if read_only.admits(&name) {
                assert!(full.admits(&name), "{name}: read-only ⊄ full");
            }
        }
        assert!(advertised_names(&offline).len() < advertised_names(&read_only).len());
        assert!(advertised_names(&read_only).len() < advertised_names(&full).len());
    }

    #[test]
    fn the_roster_and_the_router_are_one_derivation() {
        // ⚠ THE PROPERTY THE WHOLE FEATURE RESTS ON. An advertised roster and a routing gate
        // computed separately are two lists that can disagree — a tool advertised and then refused
        // wastes an agent's turn, and one WITHHELD but still routed is an ungated write. Checked
        // for every ring AND with a `--deny-tool` layered on, because the subtraction is the case
        // where a second mechanism would most plausibly have been introduced.
        for profile in [Profile::Full, Profile::ReadOnly, Profile::Offline] {
            let access = ToolAccess::new(profile, vec!["list_series".to_string()]);
            let advertised = advertised_names(&access);
            for name in served_tool_names() {
                assert_eq!(
                    advertised.contains(&name),
                    access.admits(&name),
                    "{name} under {profile:?}: tools/list and tools/call disagree"
                );
            }
            assert!(!access.admits("list_series"), "a denied tool is withheld under every ring");
        }
    }

    #[test]
    fn a_write_tool_is_refused_under_read_only_and_the_refusal_names_the_profile() {
        let mut s = server_under(Profile::ReadOnly);
        let err = s
            .call_tool(
                "submit_order",
                &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }),
            )
            .unwrap_err();
        assert!(err.refused, "a scope refusal is a GATE saying no, and the transcript reads that");
        assert!(err.message.contains("read-only"), "the profile must be named: {err:?}");
        assert!(err.message.contains("--profile full"), "…and how to change it: {err:?}");
        assert!(err.message.contains("node_snapshot"), "…and what IS served: {err:?}");
        // Nothing was previewed either: the gate runs BEFORE the command is built, so a refused
        // write leaves no token behind to be confirmed later.
        assert!(s.pending.by_token.is_empty(), "a refused write must mint nothing");
    }

    #[test]
    fn a_denied_tool_is_subtracted_from_whatever_the_profile_allows() {
        // ONE mechanism, not a second one: the deny list feeds the same pass the profile does.
        let access = ToolAccess::new(Profile::Full, vec!["market_exit".to_string()]);
        assert!(!access.admits("market_exit"));
        assert!(access.admits("submit_order"), "the rest of the ring is untouched");
        assert!(!advertised_names(&access).contains(&"market_exit".to_string()));
        let mut s = Server { access, ..test_server() };
        let err = s.call_tool("market_exit", &json!({})).unwrap_err();
        assert!(err.refused);
        assert!(err.message.contains("--deny-tool market_exit"), "got: {err:?}");
    }

    #[test]
    fn an_unknown_profile_is_refused_and_never_widens_to_full() {
        // ⚠ THE FAILURE THIS PINS is the one the rival surface gets right and a `_ => Full` arm
        // gets catastrophically wrong: a typo in a launch config that silently serves the write
        // tools to an agent the operator believed was scoped down.
        let err = Profile::parse("readonly").unwrap_err();
        for name in profile_names() {
            assert!(err.contains(name), "the error must name every valid profile: {err}");
        }
        let refused = parse_config(["--profile", "readonly"].map(String::from).into_iter())
            .expect_err("an unknown profile must not parse");
        assert!(refused.contains("read-only"), "got: {refused}");
        // …and the whole invocation is refused rather than started, so there is no server to widen.
        let ok = parse_config(["--profile", "read-only"].map(String::from).into_iter())
            .expect("a real profile parses");
        assert_eq!(ok.access.profile_name(), "read-only");
    }

    #[test]
    fn an_unknown_deny_tool_is_a_usage_error_naming_the_roster() {
        // A silently-ignored `--deny-tool` is an operator believing a tool is gone when it is not.
        let err = parse_config(["--deny-tool", "submit-order"].map(String::from).into_iter())
            .expect_err("a name this server does not serve must not parse");
        assert!(err.contains("submit_order"), "the roster must be quoted back: {err}");
        let ok = parse_config(["--deny-tool", "submit_order"].map(String::from).into_iter())
            .expect("a served name parses");
        assert!(!ok.access.admits("submit_order"));
    }

    #[test]
    fn every_resource_is_a_way_in_to_a_named_tool() {
        // The mapping the resource scope gate reads, held equal to the advertised resources in BOTH
        // directions — a third resource with no row here would silently escape the profile.
        let spec = resources_spec();
        let uris: Vec<&str> =
            spec.as_array().unwrap().iter().map(|r| r["uri"].as_str().unwrap()).collect();
        for (uri, tool) in RESOURCE_TOOLS {
            assert!(uris.contains(&uri), "RESOURCE_TOOLS names {uri}, which is not advertised");
            assert!(
                served_tool_names().iter().any(|t| t == tool),
                "{uri} claims to be a way in to {tool}, which this server does not serve"
            );
        }
        for uri in &uris {
            assert!(
                RESOURCE_TOOLS.iter().any(|(u, _)| u == uri),
                "{uri} is advertised with no tool row — it would escape the profile gate"
            );
        }
    }

    #[test]
    fn a_resource_is_withheld_exactly_when_the_tool_it_reaches_is() {
        // ⚠ A resource is a second WAY IN to a tool's implementation. Under `offline`, serving
        // `vike://node/snapshot` would hand back the answer of a tool the profile refused.
        let mut offline = server_under(Profile::Offline);
        let listed = offline.handle(&req(1, "resources/list", json!({}))).unwrap();
        assert_eq!(listed["result"]["resources"], json!([]), "got {listed}");
        let read = offline
            .handle(&req(2, "resources/read", json!({ "uri": "vike://node/snapshot" })))
            .unwrap();
        assert_eq!(read["error"]["code"], -32002, "got {read}");
        assert!(read["error"]["message"].as_str().unwrap().contains("offline"), "got {read}");

        // …and `read-only` withholds NEITHER, because neither resource is a write. That is the
        // non-vacuity half: without it the assertions above would pass on a gate that refused
        // every resource under every profile.
        let mut read_only = server_under(Profile::ReadOnly);
        let listed = read_only.handle(&req(3, "resources/list", json!({}))).unwrap();
        assert_eq!(listed["result"]["resources"].as_array().unwrap().len(), RESOURCE_TOOLS.len());
    }

    #[test]
    fn a_scoped_prompt_says_the_writes_are_absent_instead_of_teaching_them() {
        let mut s = server_under(Profile::ReadOnly);
        let got =
            s.handle(&req(1, "prompts/get", json!({ "name": "triage_a_stuck_order" }))).unwrap();
        let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.starts_with("⚠ THIS SESSION SERVES NO ORDER-WRITE TOOL"), "got: {text}");
        assert!(text.contains("read-only"), "the banner names the profile: {text}");
        assert!(
            !text.contains("preview_token"),
            "a sheet must not teach a two-call gate for tools it has just said are absent: {text}"
        );
        // The READ half of the sheet survives — the prompt is still the right procedure, minus the
        // acting.
        assert!(text.contains("node_snapshot"), "got: {text}");
        // …and under the default profile the banner is absent entirely (byte-identical teaching).
        let mut full = server();
        let got =
            full.handle(&req(2, "prompts/get", json!({ "name": "triage_a_stuck_order" }))).unwrap();
        let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(!text.contains("SERVES NO ORDER-WRITE TOOL"), "got: {text}");
        assert!(text.contains("preview_token"));
    }

    // ---- the agent transcript (`--trace`) ----------------------------------------------------

    fn scratch(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new().prefix(tag).tempdir().expect("a scratch directory")
    }

    /// A server that RECORDS, writing into `dir`. Everything else is [`test_server`]'s — no node,
    /// no credentials — so nothing here can reach a venue whatever the transcript says.
    fn tracing_server(dir: &std::path::Path, profile: Profile) -> Server {
        Server {
            access: ToolAccess::new(profile, Vec::new()),
            trace: Some(McpTrace::new(dir.to_path_buf())),
            ..test_server()
        }
    }

    /// Every record written under `dir`, parsed. Reads what is ON DISK rather than what the writer
    /// returned: the claim is that the transcript survives, not that a function was called.
    fn records(dir: &std::path::Path) -> Vec<Value> {
        let mut out = Vec::new();
        let entries = std::fs::read_dir(dir).expect("the transcript directory exists");
        let mut paths: Vec<std::path::PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("a readable record file");
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                out.push(serde_json::from_str(line).expect("one JSON object per line"));
            }
        }
        out
    }

    #[test]
    fn a_refused_write_is_recorded_with_the_reason_it_was_refused() {
        // ⚠ THE WHOLE POINT OF THE FILE. A refusal reaches the NODE's audit trail never — it was
        // refused before a byte left this process — so if it is not here it is nowhere.
        let dir = scratch("vike-mcp-refused");
        let mut s = tracing_server(dir.path(), Profile::ReadOnly);
        let resp = s
            .handle(&req(
                1,
                "tools/call",
                json!({ "name": "submit_order", "arguments": { "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 } }),
            ))
            .unwrap();
        assert_eq!(resp["result"]["isError"], true, "got {resp}");
        let recorded = records(dir.path());
        assert_eq!(recorded.len(), 1, "one call, one record: {recorded:?}");
        assert_eq!(recorded[0]["tool"], json!("submit_order"));
        assert_eq!(recorded[0]["verdict"], json!("refused"));
        assert_eq!(recorded[0]["write"], json!(true));
        assert_eq!(recorded[0]["profile"], json!("read-only"));
        assert!(
            recorded[0]["detail"].as_str().unwrap().contains("read-only"),
            "the reason must be recorded, not just the fact: {recorded:?}"
        );
    }

    /// **THE DECLARED BOUND on what a `set_setting` transcript can say**, pinned in both
    /// directions so a green run means it was measured rather than assumed.
    ///
    /// `crate::cmd::mcp_trace` redacts an argument whose NAME is credential-shaped, and
    /// `vike_config::is_secret_key` — the workspace's one authority for that — matches the bare
    /// word `KEY`. So the `key` argument is recorded redacted, and no spelling of that concept
    /// escapes it (`setting_key`, `dotted_key`: all `_KEY`). The fix is NOT a second table here;
    /// what the surface owes instead is to say what the record still carries.
    #[test]
    fn the_transcript_redacts_the_settings_key_and_keeps_the_rest() {
        let dir = scratch("vike-mcp-setting");
        let mut s = tracing_server(dir.path(), Profile::Full);
        let args = json!({
            "file": "config.toml",
            "key": "config.tradehub_addr",
            "value": "127.0.0.1:7879"
        });
        s.handle(&req(1, "tools/call", json!({ "name": "set_setting", "arguments": args })))
            .unwrap();
        let recorded = records(dir.path());
        assert_eq!(recorded.len(), 1, "one call, one record: {recorded:?}");
        assert_eq!(recorded[0]["tool"], json!("set_setting"));
        assert_eq!(recorded[0]["write"], json!(true), "a lifecycle verb is a WRITE in the record");
        assert_eq!(recorded[0]["verdict"], json!("preview"), "the first call sends nothing");
        // The bound…
        assert_ne!(
            recorded[0]["args"]["key"],
            json!("config.tradehub_addr"),
            "`key` matches the bare `KEY` shape, so its value is redacted — see this tool's spec \
             comment for why that is not fixed here: {recorded:?}"
        );
        // …and what survives it, which is what makes the record still worth keeping.
        assert_eq!(recorded[0]["args"]["file"], json!("config.toml"), "{recorded:?}");
        assert_eq!(recorded[0]["args"]["value"], json!("127.0.0.1:7879"), "{recorded:?}");
    }

    #[test]
    fn a_preview_records_the_minted_token_and_the_confirm_records_the_presented_one() {
        let dir = scratch("vike-mcp-token");
        let mut s = tracing_server(dir.path(), Profile::Full);
        let args = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 });
        let preview = s
            .handle(&req(1, "tools/call", json!({ "name": "submit_order", "arguments": args })))
            .unwrap();
        let token = preview["result"]["structuredContent"]["preview_token"]
            .as_str()
            .expect("a preview mints a token")
            .to_string();
        let mut confirming = args.clone();
        confirming["confirm"] = json!(true);
        confirming["preview_token"] = json!(token.clone());
        // With no node configured this gets PAST the gate and fails at the connection, which is how
        // this suite tells "admitted" from "refused" without ever reaching a venue.
        s.handle(&req(2, "tools/call", json!({ "name": "submit_order", "arguments": confirming })))
            .unwrap();

        let recorded = records(dir.path());
        assert_eq!(recorded.len(), 2, "{recorded:?}");
        assert_eq!(recorded[0]["verdict"], json!("preview"), "a preview is not an `ok`");
        assert_eq!(recorded[0]["token"], json!(token));
        assert_eq!(recorded[0]["token_role"], json!(TOKEN_MINTED));
        assert_eq!(
            recorded[1]["token"],
            json!(token),
            "the same identity, on the call that used it"
        );
        assert_eq!(recorded[1]["token_role"], json!(TOKEN_PRESENTED));
        assert_eq!(
            recorded[1]["verdict"],
            json!("error"),
            "an unreachable node is an ERROR, not a gate refusal: {recorded:?}"
        );
        // ⚠ The token is recorded as an IDENTITY in its own field and NOT copied out of the
        // arguments — where the credential-name rule redacts it like any other `_TOKEN`.
        assert_eq!(recorded[1]["args"]["preview_token"], json!(mcp_trace::REDACTED));
        // The order's own fields ARE recorded: what the agent attempted is the point.
        assert_eq!(recorded[1]["args"]["qty"], json!(1.0));
    }

    #[test]
    fn a_read_tool_is_recorded_too() {
        // Non-vacuity for the write assertions above: the transcript is a record of the SESSION,
        // not a second copy of the order log.
        let dir = scratch("vike-mcp-read");
        let mut s = tracing_server(dir.path(), Profile::Full);
        s.handle(&req(1, "tools/call", json!({ "name": "list_templates", "arguments": {} })))
            .unwrap();
        let recorded = records(dir.path());
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0]["tool"], json!("list_templates"));
        assert_eq!(recorded[0]["verdict"], json!("ok"));
        assert_eq!(recorded[0]["write"], json!(false));
        assert_eq!(recorded[0]["token"], Value::Null, "a read mints and presents no token");
        assert_eq!(recorded[0]["token_role"], Value::Null);
    }

    #[test]
    fn a_planted_secret_in_an_argument_never_reaches_the_transcript() {
        // Two shapes, because the rule has two positions: a credential-shaped KEY, and a credential
        // NAMED inside free text. `crate::cmd::mcp_trace` owns the rule; this proves it is actually
        // wired into the server's own path rather than only unit-tested beside it.
        let dir = scratch("vike-mcp-secret");
        let mut s = tracing_server(dir.path(), Profile::Full);
        s.handle(&req(
            1,
            "tools/call",
            json!({ "name": "submit_order", "arguments": {
                "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0,
                "api_secret": "PLANTED-ARGUMENT-SECRET",
                "reason": "using BINANCE_LIVE_API_SECRET=PLANTED-INLINE-SECRET"
            }}),
        ))
        .unwrap();
        let recorded = records(dir.path());
        let line = recorded[0].to_string();
        assert!(!line.contains("PLANTED-ARGUMENT-SECRET"), "a keyed secret reached disk: {line}");
        assert!(!line.contains("PLANTED-INLINE-SECRET"), "an inline secret reached disk: {line}");
        assert_eq!(recorded[0]["args"]["api_secret"], json!(mcp_trace::REDACTED));
        assert_eq!(recorded[0]["args"]["reason"], json!(mcp_trace::REDACTED));
    }

    #[test]
    fn without_the_flag_the_transcript_writes_nothing_at_all() {
        // ⚠ Not "writes an empty file" — creates NOTHING. The default must leave no trace on an
        // operator's disk, because the default is what every existing MCP client config runs.
        let dir = scratch("vike-mcp-off");
        let mut s = test_server();
        assert!(s.trace.is_none(), "the default server records nothing");
        s.handle(&req(1, "tools/call", json!({ "name": "list_templates", "arguments": {} })))
            .unwrap();
        s.handle(&req(
            2,
            "tools/call",
            json!({ "name": "submit_order", "arguments": { "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 } }),
        ))
        .unwrap();
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert!(left.is_empty(), "an untraced session wrote {} entries", left.len());
    }

    #[test]
    fn the_transcript_survives_a_restart_and_a_second_session_appends_to_it() {
        // Two servers over one directory, which is BOTH the restart shape and the two-agents shape.
        let dir = scratch("vike-mcp-restart");
        let mut first = tracing_server(dir.path(), Profile::Full);
        first
            .handle(&req(1, "tools/call", json!({ "name": "list_templates", "arguments": {} })))
            .unwrap();
        drop(first);
        let mut second = tracing_server(dir.path(), Profile::ReadOnly);
        second
            .handle(&req(1, "tools/call", json!({ "name": "list_indicators", "arguments": {} })))
            .unwrap();
        let recorded = records(dir.path());
        assert_eq!(recorded.len(), 2, "the second session APPENDED: {recorded:?}");
        assert_eq!(recorded[0]["tool"], json!("list_templates"), "the first record survived");
        assert_eq!(recorded[1]["tool"], json!("list_indicators"));
        assert_eq!(recorded[1]["profile"], json!("read-only"), "each record names its own scope");
    }

    #[test]
    fn a_trace_that_cannot_resolve_a_project_refuses_rather_than_recording_nowhere() {
        // ⚠ The asymmetry with `vike_boot::journal_boot_settings`, which records nothing when there
        // is no project: there NOBODY asked, here the operator TYPED `--trace`, and a server that
        // started anyway would be discovered to have recorded nothing after the incident.
        let err = resolve_trace(TraceRequest::ProjectState, None).expect_err("no project, no home");
        assert!(err.contains("--trace-dir"), "the refusal must name a way out: {err}");
        // Spelled without its `VIKE_` head deliberately: `crates/vike-ops/tests/settings_registry.rs`
        // harvests env-shaped string literals out of `src/` and demands a `SETTINGS` row for each,
        // keyed on `(name, krate)` — and this crate has no row for that variable, because it reads
        // it through `vike-boot` rather than itself.
        assert!(err.contains("SETTINGS_DIR"), "…and the other one: {err}");
        // The named-directory form is honourable with no project at all.
        let dir = scratch("vike-mcp-resolve");
        let resolved = resolve_trace(TraceRequest::Dir(dir.path().to_path_buf()), None)
            .expect("an explicit directory needs no project");
        assert_eq!(resolved.expect("a writer").dir(), dir.path());
        // …and no flag is no writer, whatever the project situation is.
        assert!(resolve_trace(TraceRequest::Off, None).expect("off is not a failure").is_none());
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

    /// **THE DATA DELETER KEEPS EVERY GUARD.** A POSITIVE gate, and it replaces an EXCLUSION.
    ///
    /// The design that proposed `delete_series` opened by forbidding it — a model must not delete
    /// data — and its gate would have been an ABSENCE check over `tools_spec` (no advertised tool
    /// may contain `delete`/`remove`/`purge`/…). The owner reversed that on 2026-09-07: a user must
    /// be able to delete through both the CLI and this surface. **An absence gate cannot be adapted
    /// to a present capability**, so the shape is inverted: the tool exists, and this holds every
    /// guard that made permitting it defensible.
    ///
    /// §9.1's grant argument survives the reversal and is what sets the bar: an irreversible delete
    /// of market history is a larger grant than an order, and in one way larger than a credential
    /// write — a credential can be reissued at the venue, and a deleted tape whose venue no longer
    /// serves that window cannot be re-fetched at all. Five evidences, because losing any ONE of
    /// them is a different failure:
    ///
    /// 1. **`delete_series` is in [`WRITE_TOOLS`].** Removing it would silently drop the mandatory
    ///    preview, the `destructiveHint`, the `read-only` withholding and the transcript's write
    ///    classification IN ONE EDIT. That is the failure this evidence exists for, and it is why
    ///    the roster membership is asserted rather than inferred from the annotations.
    /// 2. **A call without `confirm: true` mutates NOTHING** — asserted by driving the real
    ///    [`Server`] against a real datahub over a RECORDING store, and checking the store saw no
    ///    delete. NOT by inspecting the returned text: a preview that SAYS it changed nothing while
    ///    having changed something is exactly what a text assertion cannot catch.
    /// 3. **A token bound to a DIFFERENT deletion is refused** — the binding exercised rather than
    ///    assumed, and over `produced_by` as well as the selector, because the assertion decides
    ///    what actually goes.
    /// 4. **`produced_by` is required by the schema**, and a call omitting it is refused BEFORE any
    ///    store is opened.
    /// 5. **It is absent from `tools/list` under both `read-only` and `offline`**, asserted against
    ///    the real [`ToolAccess`] rings rather than against a hand-written expectation.
    #[test]
    fn the_data_deleter_keeps_every_guard() {
        // ---- 1. the roster membership, which is the four other guards in one line ---------------
        assert!(is_write_tool("delete_series"), "delete_series must route as a write");
        assert!(WRITE_TOOLS.contains(&"delete_series"), "…and be ON the roster that says so");
        let spec = tools_spec();
        let tool = spec
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "delete_series")
            .expect("delete_series is advertised");
        assert_eq!(tool["annotations"]["destructiveHint"], json!(true));
        assert_eq!(tool["annotations"]["readOnlyHint"], json!(false));
        assert_eq!(
            tool["annotations"]["idempotentHint"],
            json!(true),
            "the underlying delete IS idempotent, and saying so is honest: a retry after a dropped \
             reply does not double-delete"
        );
        assert_eq!(
            tool["annotations"]["openWorldHint"],
            json!(true),
            "it reaches a datahub over a socket — which is also what withholds it from `offline`"
        );

        // ---- 4. the schema requires the assertion, and the router refuses without it ------------
        let required: Vec<&str> = tool["inputSchema"]["required"]
            .as_array()
            .expect("delete_series declares required properties")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            required.contains(&"produced_by"),
            "produced_by must be REQUIRED by the schema, unconditionally: {required:?}"
        );
        let resp = call("delete_series", json!({ "kind": "bar", "venue": "binance" }));
        assert_eq!(resp["result"]["isError"], true, "a call with no produced_by must be refused");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("REQUIRED"), "the refusal must say so: {text}");
        assert!(
            resp["result"]["structuredContent"].is_null(),
            "a refusal is not a preview and must mint no token: {resp}"
        );

        // ---- 5. the rings withhold it, off the REAL ToolAccess ---------------------------------
        for profile in [Profile::ReadOnly, Profile::Offline] {
            let access = ToolAccess::new(profile, Vec::new());
            assert!(!access.admits("delete_series"), "{profile:?} must withhold the deleter",);
            assert!(
                !advertised_names(&access).iter().any(|n| n == "delete_series"),
                "{profile:?} must not ADVERTISE it either"
            );
        }
    }

    /// A datahub that ADVERTISES the delete verb and RECORDS every `dry_run` flag it is sent,
    /// answering each with a fixed one-series plan.
    ///
    /// ⚠ It speaks the wire rather than wrapping a store, and that is what makes the proof
    /// possible at all: the property under test is "a preview SENDS a dry run", which is a fact
    /// about the REQUEST — and the store this server would delete from lives in another process on
    /// another box, so "the series is still on disk" is not a thing this crate can look at. A
    /// `HistStore` double would have proved the same thing one layer further away, through eighteen
    /// delegating methods.
    ///
    /// Key-LESS on purpose (no `FEATURE_AUTH`, no nonce), so the plain `DatahubClient::connect`
    /// reaches it — this fixture's `Server` is built with `datahub_keys: None`, which is the arm
    /// that fallback belongs to.
    fn spawn_recording_datahub() -> (std::net::SocketAddr, Arc<Mutex<Vec<bool>>>) {
        use std::net::TcpListener;
        use vike_datahub_client::proto::{
            DeleteDone, PROTO_VERSION, RemovalOutcome, RemovalPlan, Request, Response,
            SeriesSelector, read_frame, write_frame,
        };

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let seen: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let recorded = Arc::clone(&recorded);
                std::thread::spawn(move || {
                    while let Ok(request) = read_frame::<_, Request>(&mut s) {
                        let response = match request {
                            Request::Hello { .. } => Response::Welcome {
                                proto_version: PROTO_VERSION,
                                features: vec!["delete_series".to_string()],
                                nonce: None,
                            },
                            Request::DeleteSeries { dry_run, .. } => {
                                recorded.lock().unwrap().push(dry_run);
                                let plan = RemovalPlan {
                                    selector: SeriesSelector::new("bar", "binance"),
                                    produced_by: Some("klines:".to_string()),
                                    series: Vec::new(),
                                };
                                Response::Deleted(DeleteDone {
                                    plan,
                                    outcome: (!dry_run).then(RemovalOutcome::default),
                                })
                            }
                            other => Response::Error(format!("unexpected {other:?}")),
                        };
                        if write_frame(&mut s, &response).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        (addr, seen)
    }

    /// **Evidences 2 and 3 of [`the_data_deleter_keeps_every_guard`]**, driven end to end against a
    /// real server and a real wire.
    ///
    /// 2. a call WITHOUT `confirm` sends `dry_run: true` and NOTHING else — the mutation gate, and
    ///    it is asserted on what left the process rather than on what the answer says about itself;
    /// 3. a token bound to a DIFFERENT deletion is refused — including one that differs only in
    ///    `produced_by`, which is the argument that decides what actually goes.
    #[test]
    fn a_delete_preview_sends_only_a_dry_run_and_its_token_is_bound_to_it() {
        let (addr, sent) = spawn_recording_datahub();
        let mut s = Server { datahub_addr: addr.to_string(), ..test_server() };
        let args = json!({
            "kind": "bar", "venue": "binance", "symbol": "BTCUSDT", "interval": "1h",
            "produced_by": "klines:"
        });

        // ---- 2. the PREVIEW ---------------------------------------------------------------------
        let resp = s
            .handle(&req(1, "tools/call", json!({ "name": "delete_series", "arguments": args })))
            .unwrap();
        let body = &resp["result"]["structuredContent"];
        assert_eq!(body["will_execute"], json!(false), "{resp}");
        let token = body["preview_token"].as_str().expect("a plan mints a token").to_string();
        assert_eq!(
            *sent.lock().unwrap(),
            vec![true],
            "a preview must send EXACTLY ONE request, and it must be a dry run"
        );

        // ---- 3a. the SAME token against a DIFFERENT SELECTOR ------------------------------------
        let mut other = args.clone();
        other["symbol"] = json!("ETHUSDT");
        other["confirm"] = json!(true);
        other["preview_token"] = json!(token);
        let resp = s
            .handle(&req(2, "tools/call", json!({ "name": "delete_series", "arguments": other })))
            .unwrap();
        assert_eq!(resp["result"]["isError"], true, "{resp}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("does not match this deletion"), "{text}");
        assert_eq!(
            *sent.lock().unwrap(),
            vec![true],
            "a refused confirm must send NOTHING — the token is consumed before the socket"
        );

        // ⚠ The token is spent by that attempt (single use), so the next case takes a fresh one.
        let resp = s
            .handle(&req(3, "tools/call", json!({ "name": "delete_series", "arguments": args })))
            .unwrap();
        let token =
            resp["result"]["structuredContent"]["preview_token"].as_str().unwrap().to_string();

        // ---- 3b. the same SELECTOR under a DIFFERENT ASSERTION ----------------------------------
        let mut reasserted = args.clone();
        reasserted["produced_by"] = json!("pmxt:");
        reasserted["confirm"] = json!(true);
        reasserted["preview_token"] = json!(token.clone());
        let resp = s.handle(&req(
            4,
            "tools/call",
            json!({ "name": "delete_series", "arguments": reasserted }),
        ));
        let resp = resp.unwrap();
        assert_eq!(
            resp["result"]["isError"], true,
            "`produced_by` is part of the INTENT: a token previewed under one assertion must not \
             confirm a delete under another: {resp}"
        );

        // ---- ...and the CONFIRMING call, which is the non-vacuity proof for all of the above ----
        let resp = s
            .handle(&req(5, "tools/call", json!({ "name": "delete_series", "arguments": args })))
            .unwrap();
        let token =
            resp["result"]["structuredContent"]["preview_token"].as_str().unwrap().to_string();
        let mut confirming = args.clone();
        confirming["confirm"] = json!(true);
        confirming["preview_token"] = json!(token);
        let resp = s.handle(&req(
            6,
            "tools/call",
            json!({ "name": "delete_series", "arguments": confirming }),
        ));
        let resp = resp.unwrap();
        assert_eq!(resp["result"]["isError"], false, "{resp}");
        assert_eq!(resp["result"]["structuredContent"]["will_execute"], json!(true));
        let flags = sent.lock().unwrap().clone();
        assert_eq!(
            flags.last(),
            Some(&false),
            "only a CONFIRMED call may send a non-dry-run request: {flags:?}"
        );
        assert_eq!(
            flags.iter().filter(|d| !**d).count(),
            1,
            "…and exactly one of them did, out of {} requests: {flags:?}",
            flags.len()
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

        // …and the THIRD half, added with the `instructions` field: a second free-text channel that
        // reaches the model, which the two scans above cannot see. `instructions` is not a tool, so
        // `0036`'s reopen clause ("an MCP TOOL that writes, or reaches, a credential") does not
        // cover it — which is exactly why it needs its own line here rather than an assumption.
        //
        // ⚠ The SUBJECT×ACT pairing above CANNOT be reused on this text, and the attempt is
        // instructive: `<project>/settings/secrets.env` pairs the subject `secret` with the act
        // `set` — inside the word `settings` — so the crude rule would refuse the surface for
        // naming the store's real path. Substring matching is right for a tool NAME and a one-line
        // description written to a house style; it is wrong for prose. The rule here is the precise
        // one instead: every `secrets` subcommand this text names must be a READ.
        const SECRETS_READ_VERBS: [&str; 2] = ["list", "path"];
        for text in [
            instructions(&ToolAccess::full()),
            instructions(&ToolAccess::new(Profile::ReadOnly, Vec::new())),
        ] {
            for command in backticked_commands(&text) {
                let mut words = command.split_whitespace();
                if words.next() != Some("vike-cli") || words.next() != Some("secrets") {
                    continue;
                }
                let sub = words.next().unwrap_or_default();
                assert!(
                    SECRETS_READ_VERBS.contains(&sub),
                    "the MCP instructions name `vike-cli secrets {sub}`, which is not one of the \
                     READ subcommands {SECRETS_READ_VERBS:?}. The instructions are text an agent \
                     acts on, and this surface directs the operator to READ a credential and never \
                     to write one — see docs/decisions/0036, whose four reasons a credential write \
                     reachable from an agent's context would have to argue against."
                );
            }
            // …and the POSITIVE half, because an omission is what an agent fills in with a guess:
            // the text must SAY this server cannot reach a credential at all.
            assert!(
                text.contains(INSTRUCTIONS_NO_CREDENTIAL),
                "the MCP instructions must state outright that this server reaches no credential \
                 — an omission is what an agent fills in with a guess, and the store's path reads \
                 as an invitation without it"
            );
        }
    }

    /// A `#[cfg(test)]` helper, not a parser: every command the instructions name is inside
    /// backticks, so the spans ARE the commands and nothing has to guess where one ends.
    fn backticked_commands(text: &str) -> Vec<&str> {
        text.split('`').skip(1).step_by(2).collect()
    }

    #[test]
    fn initialize_carries_instructions_that_name_the_surface_beyond_this_one() {
        let mut s = server();
        let init = s.handle(&req(1, "initialize", json!({}))).unwrap();
        let text = init["result"]["instructions"].as_str().expect(
            "initialize must carry an `instructions` string — it is the ONE channel that can say \
             what this surface is NOT, and a client shows it to the model",
        );
        // The four capabilities that live outside this server, each measured as MISSING by the
        // 2026-09-06 model run. Named here rather than derived because the LIST is the editorial
        // decision this text exists to make; that each one is REAL is what the next test derives.
        for needle in
            ["vike-backend datahub --record", "vike-cli data fetch", "vike-cli backtest --local"]
        {
            assert!(text.contains(needle), "the instructions must name `{needle}`");
        }
        assert!(text.contains("settings/secrets.env"), "the instructions must name the ONE store");
    }

    #[test]
    fn the_instructions_are_scoped_by_the_same_access_the_roster_is() {
        // The write clause and the `list_series` clause are ADVERTISEMENTS of tools. A profile that
        // withholds the tool must withhold the sentence, or the prose is advertising what
        // `tools/list` refuses — the exact drift [`ToolAccess`] exists to make impossible.
        let full = instructions(&ToolAccess::full());
        assert!(full.contains("PREVIEW-GATED"), "`full` serves the write tools and must say so");
        assert!(full.contains("list_series"), "`full` serves the datahub reads and may say so");

        let read_only = instructions(&ToolAccess::new(Profile::ReadOnly, Vec::new()));
        assert!(
            !read_only.contains("PREVIEW-GATED"),
            "`read-only` serves no write tool, so the preview gate is a door the agent cannot reach"
        );
        assert!(read_only.contains("list_series"), "`read-only` still serves the datahub reads");

        let offline = instructions(&ToolAccess::new(Profile::Offline, Vec::new()));
        assert!(!offline.contains("PREVIEW-GATED"), "`offline` serves no write tool");
        assert!(
            !offline.contains("list_series"),
            "`offline` withholds every network tool, `list_series` included — telling an agent to \
             confirm a fetch with it is advertising a withheld tool in prose"
        );

        // What NO profile may drop: the operator-side commands. They are things a HUMAN types, and
        // no scoping of THIS server changes what the operator can run on the box.
        for (name, text) in [("full", &full), ("read-only", &read_only), ("offline", &offline)] {
            assert!(
                text.contains("vike-backend datahub --record")
                    && text.contains("settings/secrets.env"),
                "the `{name}` profile dropped an OPERATOR-side command; the profile scopes this \
                 server's tools, not the operator's terminal"
            );
        }
    }

    #[test]
    fn the_instructions_name_only_real_commands() {
        // The rot the whole repository is organised against: prose that names a command the binary
        // does not have. It is worse than naming none — it sends an operator to a terminal to type
        // something that fails, on the word of the surface itself.
        //
        // Each `vike-cli` verb is checked against `crate::COMMANDS`, the dispatcher's own roster,
        // and each SUBCOMMAND and FLAG against the owning module's own `USAGE`. Neither is a copy.
        //
        // ⚠ Two bounds, declared rather than implied. Arguments are NOT checked (`run.toml`,
        // `binance:BTCUSDT:1h` and `180` are values an operator supplies, not names this workspace
        // owns), and a NON-`vike-cli` binary is not checked here at all — this crate cannot see
        // another crate's manifest, so `crates/vike-ops/tests/mcp_instructions_gate.rs` holds
        // `vike-backend` to a real `[[bin]]` from outside.
        fn usage_for(verb: &str) -> Option<&'static str> {
            match verb {
                "data" => Some(crate::cmd::data::USAGE),
                "secrets" => Some(crate::cmd::secrets::USAGE),
                "backtest" => Some(crate::cmd::backtest::USAGE),
                _ => None,
            }
        }

        let verbs: Vec<&str> = crate::COMMANDS.iter().map(|(name, _)| *name).collect();
        let mut checked = 0usize;
        for access in [ToolAccess::full(), ToolAccess::new(Profile::Offline, Vec::new())] {
            let text = instructions(&access);
            for command in backticked_commands(&text) {
                let mut words = command.split_whitespace();
                if words.next() != Some("vike-cli") {
                    continue;
                }
                // A span that is JUST the binary name (`vike-cli`, naming the product rather than
                // invoking it) carries no verb to check. It is not unchecked: the BINARY half is
                // `crates/vike-ops/tests/mcp_instructions_gate.rs`'s, from where a manifest is
                // visible.
                let Some(verb) = words.next() else { continue };
                assert!(
                    verbs.contains(&verb),
                    "the MCP instructions name `vike-cli {verb}`, which is not a registered \
                     subcommand. The dispatcher's roster is {verbs:?}."
                );
                let usage = usage_for(verb).unwrap_or_else(|| {
                    panic!(
                        "the MCP instructions name `vike-cli {verb}` and this test has no `USAGE` \
                         to hold its subcommands and flags to — add the arm, do not delete the \
                         check"
                    )
                });
                for word in words {
                    // Values are the operator's, not ours (see the bound above).
                    if !word.starts_with("--")
                        && (word.contains(['.', ':']) || word.starts_with('$'))
                    {
                        continue;
                    }
                    if word.chars().all(|c| c.is_ascii_digit()) {
                        continue;
                    }
                    assert!(
                        usage.contains(word),
                        "the MCP instructions name `{word}` under `vike-cli {verb}`, which that \
                         command's own USAGE does not offer"
                    );
                    checked += 1;
                }
            }
        }
        // Non-vacuity: a text whose backtick spans stopped parsing would pass every assertion above
        // by checking nothing at all.
        assert!(checked >= 4, "only {checked} subcommand/flag(s) were checked — the parse broke");
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
            // ...and the ROSTER, name by name. `two_call_gate` renders it from the write tools this
            // session ADMITS — which under the default `full` profile is `WRITE_TOOLS` entire; this
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
            VENUE_CHECK_UNVERIFIED,
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
            VENUE_CHECK_MOUNTED,
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
        // Over `test_server()` rather than a second field list — the same argument the `server()`
        // helper's own doc makes: a hand-written list here is a second place to forget a field, and
        // the two would then disagree about what "a fresh server" is.
        let mut s = Server {
            node_addr: Some("127.0.0.1:1".to_string()),
            keys: nodekeys::resolve(&env, &std::collections::HashMap::new(), None),
            ..test_server()
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
        assert!(first.message.contains("node"), "should reach the node step, got: {first:?}");
        assert!(!first.refused, "reaching the node is a FAILURE, not a gate refusal: {first:?}");
        // Second gets nothing — the token was removed before the caller executed.
        let second = s.call_tool("submit_order", &confirming).unwrap_err();
        assert!(
            second.message.contains("unknown or already used"),
            "a token must fire at most once, got: {second:?}"
        );
        assert!(
            second.refused,
            "a spent token is a GATE refusal, and the transcript reads that bit"
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
        let node = |c: &WireCommand| PreviewIntent::Node(Box::new(c.clone()));
        assert!(
            same_intent(&node(&a), &node(&b)),
            "a differing minted id must NOT break the binding"
        );

        // …but every other field still binds.
        let other = verbs::wire_command_for(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 50.0 }),
        )
        .unwrap();
        assert!(!same_intent(&node(&a), &node(&other)), "a different qty MUST break the binding");
        // ⚠ …and two DIFFERENT KINDS of intent never match, whatever they carry: the discriminant
        // is part of the compare, which is what stops a node token confirming a deletion.
        assert!(!same_intent(
            &node(&a),
            &PreviewIntent::Delete(DeleteIntent {
                kind: "bar".into(),
                venue: "sim".into(),
                symbol: None,
                group: None,
                interval: None,
                produced_by: "klines:".into(),
            })
        ));
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
            err.message.contains("does not match this command"),
            "a token must not confirm a DIFFERENT command, got: {err:?}"
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

    /// The venue a write NAMES, per wire variant — the input half of the gate.
    ///
    /// ⚠ The two OPTIONAL rows are the ones worth having a test for. `market_exit` and
    /// `mass_cancel` mean "every engine" when the argument is omitted, so an omission must read as
    /// "nothing to check" and never as "an empty venue to compare" — a gate that refused there
    /// would take out the panic button, which is a worse failure than the one being closed.
    #[test]
    fn the_venue_a_write_names_is_read_per_variant() {
        let of = |tool: &str, args: Value| {
            let cmd = verbs::wire_command_for(tool, &args).expect("a valid write");
            commanded_venue(&cmd).map(str::to_string)
        };
        assert_eq!(
            of("submit_order", json!({ "venue": "sim", "symbol": "B", "side": 1, "qty": 1.0 })),
            Some("sim".to_string())
        );
        assert_eq!(
            of("flatten", json!({ "venue": "sim", "symbol": "B" })),
            Some("sim".to_string())
        );
        assert_eq!(of("market_exit", json!({ "venue": "sim" })), Some("sim".to_string()));
        assert_eq!(of("mass_cancel", json!({ "venue": "sim" })), Some("sim".to_string()));
        // ...and the venue-less shapes, each of which must be checked against nothing.
        assert_eq!(of("market_exit", json!({})), None, "an omitted venue means EVERY engine");
        assert_eq!(of("mass_cancel", json!({})), None, "an omitted venue means EVERY venue");
        assert_eq!(of("cancel_order", json!({ "client_order_id": "c1" })), None);
        assert_eq!(of("modify", json!({ "client_order_id": "c1", "new_qty": 2.0 })), None);
        assert_eq!(of("set_trading_state", json!({ "state": "halted" })), None);
    }

    /// The decision, in every direction it has.
    ///
    /// ⚠ THE POSITION THIS PINS is the 2026-09-06 model run's: an agent invented `venue: "node"`
    /// from the operator's wording, and every layer under it accepted the string — the node's
    /// dry-run vets only the notional cap, and `vike_core`'s `apply_intent_routed` resolves an
    /// unroutable venue with `unwrap_or(0)`, i.e. onto the node's FIRST engine with the capability
    /// preflight skipped. A real order rested in the book under a venue that names nothing.
    #[test]
    fn a_venue_the_node_does_not_mount_is_refused_and_the_refusal_names_what_is_mounted() {
        let mounted = ["polymarket".to_string(), "binance".to_string()];
        let err = venue_verdict(Some("node"), Some(&mounted[..])).expect_err("must refuse");
        // The offending value, so the agent knows WHICH argument to change...
        assert!(err.contains("\"node\""), "the refusal must name the offending value: {err}");
        // ...what the node actually has, so it can fix it without a guess...
        assert!(err.contains("polymarket"), "the refusal must name the mounted venues: {err}");
        assert!(err.contains("binance"), "the refusal must name EVERY mounted venue: {err}");
        // ...and the tool that reports it, which is the step the failing run had skipped.
        assert!(err.contains("node_snapshot"), "the refusal must name the read tool: {err}");
    }

    /// The three passing dispositions, and the one that must NOT be a refusal.
    #[test]
    fn the_venue_check_reports_which_of_the_three_answers_it_gave() {
        let mounted = ["polymarket".to_string()];
        assert_eq!(venue_verdict(Some("polymarket"), Some(&mounted[..])), Ok(VENUE_CHECK_MOUNTED));
        assert_eq!(
            venue_verdict(None, Some(&mounted[..])),
            Ok(VENUE_CHECK_NONE),
            "nothing to check"
        );
        // ⚠ NO EVIDENCE IS NOT A REFUSAL. A node that cannot be read is a node that cannot be
        // written to either (`Server::execute` fails there), so refusing here would buy nothing and
        // would break every preview taken against an unreachable node — which is the shape
        // `mcp_transcript.rs` drives over a node-less server. What it must NOT do is present the
        // unmade check as a passed one, which is why this is its own value rather than `mounted`.
        assert_eq!(venue_verdict(Some("anything"), None), Ok(VENUE_CHECK_UNVERIFIED));
    }

    /// EXACT, never case-folded.
    ///
    /// ⚠ Routing compares the payload's venue to an engine's `route_key` with `==`
    /// (`vike_core`'s `engine_idx_for_route_key`), so `"Polymarket"` routes to nothing on a node
    /// mounting `"polymarket"` and falls back to the first engine exactly as `"node"` did. A
    /// case-insensitive refusal would therefore ADMIT a string the routing silently redirects —
    /// the hole reopened, wearing a friendlier spelling.
    #[test]
    fn the_venue_comparison_is_exact_because_the_routing_is() {
        let mounted = ["polymarket".to_string()];
        assert!(venue_verdict(Some("Polymarket"), Some(&mounted[..])).is_err(), "case matters");
        assert!(
            venue_verdict(Some("polymarket "), Some(&mounted[..])).is_err(),
            "whitespace matters"
        );
        assert_eq!(venue_verdict(Some("polymarket"), Some(&mounted[..])), Ok(VENUE_CHECK_MOUNTED));
    }

    /// A preview against a node-less server is UNVERIFIED, not refused — and it SAYS so.
    ///
    /// The end-to-end half of the disposition above, over the shipped router rather than the pure
    /// function: `test_server()` has no node, so the mounted set cannot be read, and every write
    /// tool must still preview exactly as it did before this gate existed.
    #[test]
    fn a_preview_with_no_node_reports_the_venue_as_unverified_rather_than_refusing_it() {
        let resp =
            call("submit_order", json!({ "venue": "node", "symbol": "B", "side": 1, "qty": 1.0 }));
        assert_eq!(resp["result"]["isError"], false, "no node = no evidence = no refusal: {resp}");
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["venue_check"], VENUE_CHECK_UNVERIFIED, "{sc}");
        assert!(sc["preview_token"].is_string(), "an unverified preview still mints a token: {sc}");
    }

    /// Every write tool's preview carries the field, so an agent never has to infer from its
    /// absence whether the venue was looked at.
    #[test]
    fn every_write_tool_preview_reports_its_venue_check() {
        for tool in WRITE_TOOLS {
            let resp = call(tool, every_write_tools_arguments());
            let sc = &resp["result"]["structuredContent"];
            let got = sc["venue_check"].as_str().unwrap_or_default();
            assert!(
                [VENUE_CHECK_MOUNTED, VENUE_CHECK_NONE, VENUE_CHECK_UNVERIFIED].contains(&got),
                "{tool}: every preview reports one of the three dispositions, got {sc}"
            );
        }
    }

    // ---- the node-lifecycle write tools + the two per-call node reads ------------------------

    /// [`LIFECYCLE_TOOLS`] is a SUBSET of [`WRITE_TOOLS`], never a second roster.
    ///
    /// ⚠ The direction that matters is this one: a lifecycle tool missing from `WRITE_TOOLS` loses
    /// the mandatory preview gate, the `destructiveHint` and the `read-only` withholding while
    /// still LOOKING like a gated tool — it would be routed by `call_tool`'s catch-all as an
    /// unknown tool, which is at least loud, but the annotations pin would go quiet the moment
    /// somebody "fixed" that by adding an arm.
    #[test]
    fn the_lifecycle_tools_are_a_subset_of_the_write_roster() {
        for tool in LIFECYCLE_TOOLS {
            assert!(
                WRITE_TOOLS.contains(&tool),
                "{tool} is a lifecycle tool but not on WRITE_TOOLS — it would carry no preview gate"
            );
            assert!(is_write_tool(tool), "{tool} must route as a write");
        }
        // …and each one is really BUILDABLE by the SHARED construction site, so this is not three
        // names agreeing with three names while `verbs::wire_command_for` answers `no wire command
        // for …` — the state this file's own lifecycle builder existed to avoid, and which the
        // lift into `verbs` would silently restore if one name were left behind there.
        for tool in LIFECYCLE_TOOLS {
            assert!(
                verbs::wire_command_for(tool, &every_write_tools_arguments()).is_ok(),
                "{tool}: the shared site must build a command from the shared argument union"
            );
        }
    }

    /// Each lifecycle tool inherits the MANDATORY PREVIEW: a first call sends nothing and hands
    /// back the token that would confirm it. The roster loops elsewhere prove this for
    /// `WRITE_TOOLS` entire; this is the same claim aimed at the three that are new, so a
    /// regression names them rather than "some write tool".
    #[test]
    fn a_lifecycle_write_previews_before_it_executes() {
        for tool in LIFECYCLE_TOOLS {
            let mut args = every_write_tools_arguments();
            args["confirm"] = json!(true);
            let sc = &call(tool, args)["result"]["structuredContent"];
            assert_eq!(sc["will_execute"], json!(false), "{tool}: confirm alone must not execute");
            assert!(sc["preview_token"].is_string(), "{tool}: a preview carries its token: {sc}");
            assert_eq!(sc["verified_by_node"], json!(false), "{tool}: no node, so no verdict");
        }
    }

    /// **THE TYPED-CONFIRM CONTRACT, refused half.** A `policy.toml` write with no
    /// `policy_confirm` is refused BEFORE anything is previewed — and the assertion that carries
    /// the safety is the absent token: a refusal that still minted one would be the gate handing
    /// back its own bypass.
    #[test]
    fn a_policy_write_without_the_operators_typed_confirm_is_refused_and_mints_no_token() {
        let resp = call(
            "set_setting",
            json!({
                "file": "policy.toml",
                "key": "policy.max_notional_per_order",
                "value": "250",
                "confirm": true
            }),
        );
        assert_eq!(resp["result"]["isError"], true, "got {resp}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("policy_confirm"), "the refusal must name the argument: {text}");
        assert!(
            text.contains("policy.max_notional_per_order"),
            "…and the exact key the operator has to retype: {text}"
        );
        assert!(
            text.contains("OPERATOR"),
            "…and that the retyping is the operator's, since this server holds the key and could \
             fill it in itself: {text}"
        );
        assert!(
            resp["result"]["structuredContent"].is_null(),
            "a refused write is NOT a preview and must hand back no token: {resp}"
        );
        // The bare stem is the same file, and the node accepts it — so the gate must not be
        // defeatable by dropping four characters.
        let stem = call(
            "set_setting",
            json!({ "file": "policy", "key": POLICY_KEY_FIXTURE, "value": "3" }),
        );
        assert_eq!(stem["result"]["isError"], true, "`policy` is `policy.toml`: {stem}");
    }

    /// **THE TYPED-CONFIRM CONTRACT, admitted half** — without it every assertion above would
    /// still pass if the gate refused every policy write unconditionally. With the retyped key
    /// present the call reaches the ordinary preview, and the value this server puts on the wire is
    /// the one it was HANDED.
    #[test]
    fn a_policy_write_with_the_typed_confirm_reaches_the_preview_carrying_what_it_was_given() {
        let sc = &call(
            "set_setting",
            json!({
                "file": "policy.toml",
                "key": "policy.max_notional_per_order",
                "value": "250",
                "policy_confirm": "policy.max_notional_per_order"
            }),
        )["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], json!(false), "still a preview: {sc}");
        assert!(sc["preview_token"].is_string(), "…and it mints a token: {sc}");
        assert_eq!(
            sc["wire_command"]["SetSetting"]["confirm"],
            json!("policy.max_notional_per_order"),
            "the typed confirm rides on the wire command verbatim: {sc}"
        );
    }

    // ⚠ `the_typed_confirm_is_never_derived_from_the_key` MOVED to `crate::cmd::verbs`'s tests
    // with the builder it asserts on. The claim is unchanged and it is still this server's — it is
    // the party that holds `key` and could copy it across — but the code that could do the copying
    // is now in the shared construction site, and an assertion about a builder belongs beside it.
    // What stays here is every gate in FRONT of that builder, which is the half that is genuinely
    // this surface's: the two tests above and below.

    /// A non-policy write needs no confirm and is not made to invent one.
    #[test]
    fn a_non_policy_settings_write_needs_no_typed_confirm() {
        let sc = &call(
            "set_setting",
            json!({ "file": "config.toml", "key": "config.tradehub_addr", "value": "127.0.0.1:7879" }),
        )["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], json!(false), "{sc}");
        assert!(sc["preview_token"].is_string(), "{sc}");
        assert_eq!(sc["wire_command"]["SetSetting"]["confirm"], Value::Null, "{sc}");
    }

    /// A CONFIRMED settings write leaves by [`Server::execute_settings_write`] rather than the
    /// fire-and-forget worker, and with no node configured that surfaces as the connection error —
    /// which is the positive evidence it got PAST the preview gate, the same shape
    /// `crates/vike-cli/tests/mcp_transcript.rs` uses for `submit_order`.
    #[test]
    fn a_confirmed_settings_write_reaches_the_node_step() {
        let mut s = server();
        let args = json!({
            "file": "config.toml",
            "key": "config.tradehub_addr",
            "value": "127.0.0.1:7879"
        });
        let preview = s
            .handle(&req(1, "tools/call", json!({ "name": "set_setting", "arguments": args })))
            .unwrap();
        let token = preview["result"]["structuredContent"]["preview_token"]
            .as_str()
            .expect("the preview mints a token")
            .to_string();
        let mut confirming = args.clone();
        confirming["confirm"] = json!(true);
        confirming["preview_token"] = json!(token);
        let sent = s
            .handle(&req(
                2,
                "tools/call",
                json!({ "name": "set_setting", "arguments": confirming }),
            ))
            .unwrap();
        assert_eq!(sent["result"]["isError"], true, "no node is configured: {sent}");
        let text = sent["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("no vike-tradehub node configured"),
            "a matching token must reach the node step — that is what proves the gate ADMITS: \
             {text}"
        );
    }

    // ⚠ Four PURE-BUILDER tests stood here — the mount XOR, the mount's `params` table,
    // `unmount_strategy`'s required id, and `set_setting`'s value rendering. They MOVED to
    // `crate::cmd::verbs`'s tests with the arms they exercise, unchanged in intent, and they are
    // now three of the ten the roster loops there cover rather than three of three. What stays in
    // this file is everything with a `Server` in front of it.

    /// The two per-call node reads answer with the CONNECTION problem when there is no node — never
    /// with an empty payload, which an agent would summarise as "the node runs nothing" and
    /// "the node is configured with nothing".
    #[test]
    fn a_node_read_without_a_node_is_an_error_and_never_an_empty_answer() {
        for tool in ["strategy_status", "settings_show"] {
            let resp = call(tool, json!({}));
            assert_eq!(resp["result"]["isError"], true, "{tool}: got {resp}");
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            assert!(
                text.contains("no vike-tradehub node configured"),
                "{tool}: the error must name what is missing: {text}"
            );
        }
    }

    /// The three per-call READ failure diagnoses, which an agent must be able to tell apart: two of
    /// them are permanent facts about a reachable box and retrying either forever is the behaviour
    /// the split exists to prevent.
    #[test]
    fn a_node_read_failure_says_whether_retrying_could_ever_help() {
        let unsupported = node_read_failure(
            "strategy_status",
            "1.2.3.4:7777",
            &io::Error::new(io::ErrorKind::Unsupported, "no strategy-verbs capability"),
        );
        assert!(unsupported.contains("upgrade"), "{unsupported}");
        assert!(unsupported.contains("cannot succeed"), "{unsupported}");
        assert!(
            unsupported.contains("no strategy-verbs capability"),
            "the client's own sentence \
             names the capability string and must survive: {unsupported}"
        );

        let denied = node_read_failure(
            "settings_show",
            "1.2.3.4:7777",
            &io::Error::new(io::ErrorKind::PermissionDenied, "bad signature"),
        );
        assert!(denied.contains(nodekeys::OBSERVE_KEY_ENV), "points at the key: {denied}");

        let down = node_read_failure(
            "settings_show",
            "1.2.3.4:7777",
            &io::Error::new(io::ErrorKind::ConnectionRefused, "refused"),
        );
        assert!(down.contains("cannot query"), "{down}");
        assert!(!down.contains("upgrade"), "a refused socket is not an old node: {down}");
    }

    /// An enqueue refused for want of a NODE CAPABILITY must say so in words. It used to print the
    /// bare enum name, which the lifecycle verbs made a live path: they are the first tools on this
    /// surface that a current-protocol node can decline to accept at all.
    #[test]
    fn a_capability_refusal_tells_the_agent_not_to_retry() {
        let text = control_rejection(ControlRejected::UnsupportedByNode);
        assert!(text.contains("REFUSED"), "{text}");
        assert!(text.contains("cannot succeed"), "…and that retrying is pointless: {text}");
        assert!(text.contains("upgrade"), "…and where the fix is: {text}");
        assert!(
            control_rejection(ControlRejected::Busy).contains("try again"),
            "a full queue IS transient and must read differently"
        );
    }

    /// The two new instruction clauses are scoped by the SAME [`ToolAccess`] the roster is — the
    /// identical property `the_instructions_are_scoped_by_the_same_access_the_roster_is` holds for
    /// the clauses that shipped before them.
    #[test]
    fn the_new_instruction_clauses_are_scoped_with_the_tools_they_describe() {
        let full = instructions(&ToolAccess::full());
        let read_only = instructions(&ToolAccess::new(Profile::ReadOnly, Vec::new()));
        let offline = instructions(&ToolAccess::new(Profile::Offline, Vec::new()));

        assert!(full.contains("strategy_status"), "`full` serves the node reads and may say so");
        assert!(
            read_only.contains("strategy_status"),
            "`read-only` still serves them — they authenticate under the observe scope"
        );
        assert!(
            !offline.contains("strategy_status"),
            "`offline` withholds every network tool, so naming one is advertising a withheld tool"
        );

        assert!(full.contains("policy_confirm"), "`full` serves set_setting and must teach it");
        for (name, text) in [("read-only", &read_only), ("offline", &offline)] {
            assert!(
                !text.contains("policy_confirm"),
                "`{name}` serves no lifecycle write, so the typed confirm is a door it cannot reach"
            );
        }
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
        let mut s = Server { datahub_addr: "127.0.0.1:1".to_string(), ..test_server() };
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
        let mut s = Server { datahub_addr: "127.0.0.1:1".to_string(), ..test_server() };
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
    /// beside `crates/`, and a compile-time embed would make every one of those markdown files a
    /// build input of this crate.
    ///
    /// ⚠ **The floor that keeps this from passing over a mis-resolved directory is DERIVED, and it
    /// used to be the number ten in an `assert!` and in this doc.** That is the failure the whole
    /// `skills/` tree is now generated to avoid one level down: a count written beside a set drifts
    /// from it, and this one would have been wrong the moment a skill was added — while still
    /// reading green, because it was a `>=`. So the property asserted instead is a POSITIVE one
    /// that says the same thing without naming a number: every SUBDIRECTORY of `skills/` carries a
    /// `SKILL.md`, and there is at least one. A mis-resolved path has no subdirectories and fails
    /// on the second half; a directory that lost its page fails on the first.
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
            if !entry.path().is_dir() {
                // `skills/README.md` — the generated index — sits beside the directories.
                continue;
            }
            let path = entry.path().join("SKILL.md");
            let skill = entry.file_name().to_string_lossy().to_string();
            assert!(
                path.is_file(),
                "skills/{skill} is a directory with no SKILL.md — the Agent Skills spec identifies \
                 a skill by that file, so this one installs as nothing"
            );
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
            seen > 0,
            "no skills under {} — an empty answer here is a wrong path, not a smaller package",
            dir.display()
        );
    }
}
