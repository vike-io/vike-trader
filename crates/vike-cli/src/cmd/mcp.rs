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
//! `run_walk_forward` (remote COMPUTE-daemon runs — a backtest MUTATES nothing),
//! `list_strategies` (the same daemon), `list_series` (datahub metadata), `node_snapshot`,
//! `strategy_status`, `settings_show`
//! (the two PER-CALL node reads — see [`Server::tool_strategy_status`] for why they open their own
//! short-lived connection rather than riding the held observe pipe). This is the ABSORBED `vike-mcp` tool
//! surface (Phase A of retiring that crate): the run/list tools go over a daemon's RPC verbs
//! instead of a local DataFusion store, so this CLI stays DataFusion-free; `validate_strategy` /
//! `list_templates` are offline ports (note the argument is named `script` here — vike-mcp said
//! `code` — matching this file's existing `discover_params`).
//!
//! ⚠ **This paragraph called all four run/list tools "remote datahub", and for the run verbs that
//! is not merely imprecise — it names a daemon that REFUSES them.**
//! `crates/vike-datahub/src/server.rs` answers `Request::RunParamscan` with
//! `compute_verb_moved("RunSweep")`. Those four dial [`Server::backtest_addr`], whose own doc
//! records ruling 7 and argues why it is a SECOND field rather than a second meaning for
//! `datahub_addr`. Two tools DO dial the data server — `list_series` and `delete_series`, through
//! [`Server::datahub_for_delete`] — so the plane is a property of each TOOL and a paragraph that
//! names one daemon for all six can only be half right.
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
//! one, so they moved — `verbs`' module doc carries that history.
//!
//! **`set_setting` takes a `key` and a `value`, and nothing else.** It used to take a `file` and,
//! for a policy write, a `policy_confirm` the operator had to retype; `docs/decisions/0086` made a
//! write one row named by its key and deleted the retype for every key (point 7), so both are gone
//! from the schema and the gate that demanded the second is deleted (its tombstone sits where it
//! stood). It is gated like every other write — the mandatory preview below — and its preview
//! carries `change`, the key's `old → new` read off the node ([`verbs::SettingChange`]). What an
//! agent must still do is not a gate this server can hold: an ATTENDED agent changes a live setting
//! only after the owner said yes in chat (0086 point 6), which [`INSTRUCTIONS_LIFECYCLE`] and the
//! tool's own description say, because this process cannot see the chat.
//!
//! **`--unattended` is the one place this server DOES hold that line.** A session nobody attends
//! has no chat to say yes in, and the owner ruled (decision 0040: `policy.*` on 2026-09-28, every
//! other key on 2026-09-29) that such a run changes no setting at all: under `--unattended` every
//! `set_setting` is refused before a preview or a token, naming `vike-cli config set` and the GUI
//! as where settings are changed ([`unattended_refusal`]). `crates/vike-agent-eval/src/unattended.rs`'s `mcp_argv`
//! passes the flag on every scheduled run.
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
//! gate is [`Server::vet_commanded_venue`], called from the router's `n if is_write_tool(n)` arm,
//! so a new write tool routed THROUGH that arm inherits it rather than needing a copy; that
//! function carries the mechanism it closes, read off the write path,
//! and `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md` carries the
//! disposition.
//!
//! ⚠ **This said "for every write tool" and there is ONE exception, sitting in this file, far
//! below.** [`Server::tool_delete_series`] is a NAMED arm placed ABOVE that guard, so it reaches
//! no venue check at all — and it is RIGHT not to: its selector's `venue` is a STORE PARTITION,
//! not a mount, and the node has no opinion about one. It reports [`venue_gate::VENUE_CHECK_NONE`] rather than
//! `unverified` for exactly that reason, argued at that constant. So the property to preserve is
//! narrower than "every member of [`WRITE_TOOLS`]": a write tool that is a NODE COMMAND inherits
//! the gate by joining the `is_write_tool` arm, and a write tool that is not a node command has to
//! answer the venue question itself, in its own arm, the way the deleter does. Read as written,
//! the old sentence pointed a reader at a guarantee this file's own router contradicts.
//!
//! The short version of the defect, because it is not what the layering suggests: **nothing between
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
//! call — see [`venue_gate::VENUE_CHECK_MOUNTED`]. The end-to-end proof against a REAL node lives in
//! `crates/vike-cli/src/cmd/mcp/node_drop_tests/drops.rs`'s
//! `the_venue_gate_reads_a_real_nodes_mounted_set_and_refuses_what_is_not_in_it`, and it is the
//! half that holds the EVIDENCE — `Server::mounted_venues`'s projection of the node's own frame,
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
//! `--trace` (or `--trace-dir <dir>`) turns on [`crate::cmd::mcp::trace`]: one appended JSONL record
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

// The server's opt-in agent transcript (`--trace`).
pub(crate) mod trace;

// The tool families (code-layout phase 2, task 9). Each child holds one family's `impl Server`
// block and its free helpers, and `Server::call_tool` below routes to them by name; the roster
// they all hang off (`tools_spec`) has its own file.
mod backtest_tools;
mod data_tools;
mod node_reads;
mod node_writes;
mod offline_tools;
mod tool_schemas;
mod venue_gate;

mod config;
mod instructions;
mod preview;
mod prompts;
mod protocol;
mod resources;
mod scope;

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};
use vike_model::orders::client_order_id::ClientOrderIdGenerator;
use vike_tradehub_client::{RemoteControlHandle, RemoteCoreHandle};

#[cfg(doc)]
use self::{
    instructions::INSTRUCTIONS_LIFECYCLE,
    node_writes::LIFECYCLE_TOOLS,
    prompts::two_call_gate,
    resources::{RESOURCE_TOOLS, resources_spec},
    tool_schemas::tools_spec,
};
use crate::cmd::args;
use crate::cmd::nodekeys::NodeKeyring;
use crate::cmd::verbs;
use backtest_tools::{
    tool_list_strategies, tool_run_backtest, tool_run_paramscan, tool_run_walk_forward,
};
use config::{parse_config, resolve_trace};
use data_tools::tool_list_series;
use node_writes::{preview_of, unattended_refusal};
use offline_tools::{
    tool_discover_params, tool_list_indicators, tool_list_templates, tool_validate_strategy,
};
use preview::{DeleteIntent, PendingPreviews, PreviewIntent, same_intent};
use protocol::rpc_error;
use resources::render;
use scope::{Profile, ToolAccess};
use trace::McpTrace;

/// The DATAHUB address — `list_series` and `delete_series` are the tools that dial it (overridable
/// with `--addr`).
///
/// ⚠ This read "the datahub address the `run_backtest` tool ships profiles to", which ruling 7
/// made false: that tool and its three siblings dial [`vike_config::DEFAULT_BACKTEST_ADDR`], and
/// the doc on [`Server::backtest_addr`] is where the split is argued.
///
/// ⚠ It WAS a hand copy of `"127.0.0.1:7878"`, declared as one here and deferred with "a const
/// swap is behaviour-adjacent work, not a doc fix" — and the deferral was partly forced: until
/// 2026-09-20 `DEFAULT_DATAHUB_ADDR` was not re-exported from `vike_config`'s root, so this file
/// could not have named the authority even to cite it (the citation above was to a path that did
/// not resolve). It is nameable now, and the swap is behaviour-preserving by inspection — the two
/// literals were identical — so the sibling below and this one both reach their default THROUGH
/// that crate and the client and the daemon cannot answer differently.
///
/// What a hand copy costs when the authority moves is not hypothetical here: the same shape in
/// `crates/vike-studio/src/backend/remote.rs` kept the Studio's Remote backend pointed at the data daemon
/// after ruling 7 moved every `Run*` verb off it.
const DEFAULT_ADDR: &str = vike_config::DEFAULT_DATAHUB_ADDR;
const SERVER_NAME: &str = "vike-cli";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The MCP protocol version we default to when a client does not declare one (we ECHO the client's
/// requested version on `initialize` when present). `2024-11-05` is the widely-supported baseline.
const DEFAULT_PROTOCOL: &str = "2024-11-05";
const USAGE: &str = "usage: vike-cli mcp [--addr 127.0.0.1:7878] [--backtest-addr 127.0.0.1:7880] \
                     [--node <host:port>] \
                     [--profile full|read-only|offline] [--deny-tool NAME]... \
                     [--trace | --trace-dir <dir>] [--unattended]";
/// How long a write tool waits for the node's verdict on the command it just sent before reporting
/// the outcome as unknown. A DEADLINE, not a sleep — `await_outcome` returns the instant that
/// command's reply lands — so it is generous on purpose: an agent acts on this answer, and "unknown"
/// is the expensive one.
const ACK_WAIT: Duration = Duration::from_secs(2);

/// The MCP server state: the datahub and COMPUTE-daemon addresses (two fields, not one — see
/// [`Server::backtest_addr`]), the optional vike-tradehub node address, and the two lazily-opened
/// persistent node connections (control = write, observe = read). The
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
    /// notional half is this machine's `policy.max_notional_per_order` ceiling (settings
    /// unification, Phase 5); the node's own `ControlLimits` is what actually enforces.
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
    /// means NOTHING is written and no directory is created — see [`crate::cmd::mcp::trace`].
    trace: Option<McpTrace>,
    /// The DATAHUB node keys, for the one tool that needs an authenticated connection.
    ///
    /// ⚠ **This was a DECLARED GAP until the dispatcher started filling it, and the shape of the
    /// fix is the point.** `delete_series` is served only by a KEYED datahub
    /// (`vike_datahub_client::proto`'s `FEATURE_DELETE_SERIES` carries why), so a `None` here means
    /// the plain unauthenticated `DatahubClient::connect` — which a server requiring auth refuses.
    ///
    /// It could not be closed inside `src/cmd/`: an `env::var` here would be a new `Layer::Library`
    /// row on `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` ratchet, and a
    /// credential-store read a new `CREDENTIAL_STORE_PIN` entry — both ratchets that may shrink and
    /// never grow. So the keys arrive as a PARAMETER from the dispatcher, exactly as
    /// [`NodeKeyring`] already does for the TRADEHUB pair: one call to
    /// `vike_node_proto::auth::node_keys_from_vars` over the sweep `crate::run` already
    /// owns, threaded through [`run`]. `crate::Resolved::datahub_keys` is the field that carries it.
    ///
    /// `None` remains the ordinary answer on a box that set neither key, and it still reaches the
    /// unauthenticated constructor — so a key-less datahub behaves exactly as it did before. What
    /// changed is only that a KEYED one is now reachable at all.
    datahub_keys: Option<vike_node_proto::auth::NodeKeys>,
    /// `--unattended`: nobody attends this session, so no setting may be changed through it — the
    /// owner's rulings on decision 0040 (2026-09-28 for `policy.*`, 2026-09-29 for every key).
    /// `false` (an ATTENDED session, the default) keeps `set_setting` for every key behind the
    /// ordinary preview gate.
    /// See [`unattended_refusal`].
    unattended: bool,
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
    datahub_keys: Option<vike_node_proto::auth::NodeKeys>,
    configured_backtest_addr: Option<&str>,
    configured_datahub_addr: Option<&str>,
) -> ExitCode {
    let config = match parse_config(args, configured_backtest_addr, configured_datahub_addr) {
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
        trace.prune(trace::DEFAULT_MAX_TRACE_FILES);
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
        unattended: config.unattended,
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
        backtest_addr: vike_config::DEFAULT_BACKTEST_ADDR.to_string(),
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
        // ATTENDED, like every server that predates the flag.
        unattended: false,
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
            "run_sweep" => Ok(tool_run_paramscan(&self.backtest_addr, args)?),
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
                // ── THE UNATTENDED GATE ──────────────────────────────────────────────────────
                // Beside the venue gate and for the same structural reason: BEFORE the confirm
                // branch, so a settings write in a session nobody attends is refused outright
                // rather than handed a preview and a token it could confirm with. Inert in an
                // ATTENDED session. See [`unattended_refusal`] (decision 0040, owner's rulings
                // 2026-09-28 and 2026-09-29). ⚠ The typed-confirm gate that stood here is DELETED
                // (`docs/decisions/0086` point 7) — see its tombstone beside `unattended_refusal`.
                if self.unattended {
                    unattended_refusal(&cmd).map_err(ToolError::refused)?;
                }
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
                    // A settings write's preview also shows WHAT it changes — `old → new`, read
                    // off the node (`None` for every other command).
                    let change = self.setting_change(&cmd);
                    let epoch = self.node_accounts_epoch();
                    let issued =
                        self.pending.issue(PreviewIntent::Node(Box::new(cmd.clone())), epoch);
                    return Ok(preview_of(
                        name,
                        &cmd,
                        reason.as_deref(),
                        self.caps,
                        &issued,
                        node,
                        venue_check,
                        change.as_ref(),
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
                // ⚠ **THE ACCOUNT SET MAY NOT HAVE MOVED since the preview.** The three checks
                // above answer *is this token ours*, *is it still warm*, and *was it issued for
                // THIS command* — none of them asks whether the NODE is still the node that was
                // previewed. A preview describes a routing decision, and a routing decision taken
                // against an account set that has since changed describes a node that no longer
                // exists: the engine the preview named may now be a different account's, or gone.
                //
                // This is the ONE property kept from the opaque-handle candidate the spec rejected
                // (§3.1(B)), bought for one integer, and it is what answers the ONE RULE's *"a node
                // whose account set changed since the client last looked."*
                //
                // ⚠ A `Some(0)` on both sides compares EQUAL, and that is deliberate rather than a
                // hole — see `Server::node_accounts_epoch`. A node that reports no epoch cannot
                // have moved one either, and refusing on it would break every confirm against an
                // older node while proving nothing.
                //
                // ⚠ **IT REFUSES ONLY WHEN BOTH SIDES ANSWERED, and that is a CHANGE (2026-09-19)
                // rather than the original shape.** `node_accounts_epoch` used to fold a FAILED
                // READ into `0`, so an unreadable node at confirm time read as "the set moved to
                // zero" and produced this refusal — whose every word is then false: nothing was
                // mounted, unmounted or relabelled, the link died.
                //
                // The fold did not merely misword an answer. It made this guard's verdict depend
                // on WHEN visibility was lost rather than on whether the set moved, in the
                // indefensible direction: a read failing at BOTH points stamped `0` and compared
                // `0 == 0`, so the case with LESS information PROCEEDED, while a read failing only
                // at confirm REFUSED. Treating "could not ask" as its own state makes both
                // proceed, which is the answer the `Some(0)` case has always given.
                //
                // ⚠ **The residual, stated rather than discovered later:** the read connection and
                // the control connection are separate sockets, so an unreadable epoch and a LIVE
                // control link can coexist — and this now sends with the epoch unverified. That is
                // not a new exposure; it is exactly what the `Some(0)` case above already does,
                // and it is bounded by `PREVIEW_WINDOW`. What it buys is that an agent whose READ
                // half dropped can still confirm a write its WRITE half can carry, instead of
                // being refused with a sentence about accounts that says nothing true.
                //
                // What did NOT change: two DIFFERENT answered epochs still refuse, which is the
                // whole property the spec's §3.1(B) bought.
                let epoch_now = self.node_accounts_epoch();
                if let (Some(then), Some(now)) = (previewed.accounts_epoch, epoch_now)
                    && then != now
                {
                    return Err(ToolError::refused(format!(
                        "the node's ACCOUNT SET changed since your preview ({then} -> {now}) — \
                         re-preview. The preview described which account this command would reach, \
                         and that answer was computed against a set the node no longer has: an \
                         account may have been mounted, unmounted, or relabelled in between, so \
                         the engine it named may now belong to a different account. Nothing was \
                         sent. Call this tool WITHOUT `confirm` to price it against the node as it \
                         stands now."
                    )));
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
}

/// `(name, source)` starters — re-exported from [`vike_script::TEMPLATES`], the ONE source.
///
/// ⚠ **This was a verbatim HAND COPY of `vike-studio-core/src/templates.rs` until 2026-09-18**, and
/// the reason written here was that importing that crate "would drag DataFusion into this
/// deliberately DataFusion-free CLI". That is true of `vike-studio-core` and was never true of
/// `vike-script`, which carries no DataFusion, sits at `layer = 20` (30 then), and was ALREADY a normal
/// dependency of this crate and of `vike-studio-core` alike — so the rows could always have lived
/// in a crate both sides link, and moving them cost zero packages on either side.
///
/// The re-export is deliberate public vocabulary rather than a shim: `list_templates` and
/// `vike-cli backtest templates` are one binary's two faces on one roster, and naming it once here
/// is what keeps them from disagreeing. `crates/vike-script/src/templates.rs`'s module doc carries
/// the move's full argument and what deliberately stayed behind.
pub(crate) use vike_script::TEMPLATES;

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

// The NODE-DROP suite — its own file because it stands up a real paper node behind a loopback
// relay the test owns (a harness, not one more case for the inline module below), and a child
// module rather than an integration test because it must reach `Server`'s private fields:
// `Server`'s doc argues that no PUBLIC constructor may take a node address or a key.
// ⚠ `#[path]` because the file sits BESIDE `mcp.rs` rather than under it — a non-root module
// looks in a subdirectory named after itself. `crates/vike-tradehub/src/tradehub_cli.rs`'s
// `tradehub_cli::tests::feed_splice` is the precedent. The src-walking gates find this file through the
// `#[cfg(test)]` on its declaration and resolve it the way rustc does, `#[path]` included
// (`vike_model::libm_walk::cfg_test_module_rel_files`), so the module NAME no longer has to equal
// the file STEM. It used to: until 2026-10-05 three of those gates carried a copy of the resolver
// that read no `#[path]` and derived `{dir}/NAME.rs`, and this module was `node_drop_tests` for one
// commit and resolved a file that does not exist — leaving the real one classified as LIBRARY
// code. The name was kept rather than churned back.
#[path = "mcp/node_drop_tests.rs"]
#[cfg(test)]
mod mcp_node_drop_tests;

#[path = "mcp/tests.rs"]
#[cfg(test)]
mod mcp_tests;
