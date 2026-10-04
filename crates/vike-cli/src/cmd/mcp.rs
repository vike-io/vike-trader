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
//! `crates/vike-cli/src/cmd/mcp_node_drop_tests.rs`'s
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

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_model::orders::client_order_id::ClientOrderIdGenerator;
use vike_tradehub_client::wire::WireCommand;
use vike_tradehub_client::{RemoteControlHandle, RemoteCoreHandle};

use crate::cmd::args::{self, Flags};
use crate::cmd::nodekeys::NodeKeyring;
use crate::cmd::verbs;
use backtest_tools::{
    tool_list_strategies, tool_run_backtest, tool_run_paramscan, tool_run_walk_forward,
};
use data_tools::tool_list_series;
use node_writes::{LIFECYCLE_TOOLS, preview_of, unattended_refusal};
use offline_tools::{
    tool_discover_params, tool_list_indicators, tool_list_templates, tool_validate_strategy,
};
use tool_schemas::tools_spec;
use trace::{McpTrace, TOKEN_MINTED, TOKEN_PRESENTED, Verdict};

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
/// `crates/vike-studio/src/remote.rs` kept the Studio's Remote backend pointed at the data daemon
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
    /// row on `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` ratchet, and a
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
///
/// ⚠ It ended *…of a larger system, and that is the thing no tool description can tell you* until
/// the backtest bullet in [`INSTRUCTIONS_ELSEWHERE`] had to name a datahub, and that clause is where
/// most of the bytes came from — it argued for the fact rather than stating one, and the
/// ELSEWHERE's own first line (*NAME THE COMMAND rather than refusing blind*) is what an agent acts
/// on. The fact itself, that this is a PART, is untouched.
const INSTRUCTIONS_OPENING: &str = "\
This is `vike-cli`'s agent surface: author and validate Rhai strategies, run backtests, and read \
and control one running vike-tradehub node. It is ONE surface of a larger system.";

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
///
/// ⚠ **The credential bullet keeps `settings/secrets.env` and calls the other store `a DB`, and
/// every word of that was decided by measurement rather than taste.** It used to end *One store:
/// `<project>/settings/secrets.env`*, which `docs/decisions/0054`'s credential half turned into a
/// claim about the wrong artifact on a migrated box. Three constraints then closed on the
/// replacement, each measured on the CI box after the previous one was satisfied:
///
/// 1. **The ceiling.** Spelling both stores in full cost 78 bytes and put the joined text at
///    **2067** against `mcp_instructions_gate.rs`'s 2000.
/// 2. **The eval case.** Dropping the paths for *`secrets path` names it* then failed
///    `the_four_elsewhere_cases_are_satisfiable_by_an_agent_that_read_the_instructions`:
///    `vike_agent_eval::cases`' `READ_THE_CREDENTIAL_STORE` grades an answer on naming a store, so
///    a surface carrying none would have left that check grading the model's memory of this
///    product.
/// 3. **The two tests in this module.** `initialize_carries_instructions_that_name_the_surface_beyond_this_one`
///    and `the_instructions_are_scoped_by_the_same_access_the_roster_is` both demand the literal
///    `settings/secrets.env` — so the bare basename that satisfied (1) and (2) still failed, and
///    the DIRECTORY-qualified spelling is not optional here.
///
/// Everything below `settings/secrets.env` therefore had to go: 42 bytes, which leaves 4 under the
/// ceiling. The database is named only as a KIND, and that is the honest minimum — it says the file
/// may not be what answers, and `vike-cli secrets path`, two words earlier in the same bullet, says
/// which does. The path, the per-RUN choice and the shadowing live in `skills/`, which an operator
/// installs deliberately and which no session pays for.
///
/// ⚠ **The backtest bullet read *backtest with no server: `vike-cli backtest run --local …`* until
/// decision 0084's 2026-09-25 amendment made it false**, and it was false in the way that costs an
/// operator most: the answer to "can I backtest without a server" was a flat yes. `--local` moves
/// the ENGINE, not the history — the spawned engine reads through a datahub like every other
/// reader, and `--store DIR` is refused — so on a box with files and no server the command is a
/// datahub started on them beside `--local`. That is `VIKE_DATAHUB_STORE=DIR vike-backend datahub`:
/// with no node keys it authenticates nothing and binds loopback only, which is where the engine
/// dials by default. The span starts with the environment assignment, so
/// `crates/vike-ops/tests/mcp_instructions_gate.rs`'s binary check skips that assignment to reach
/// `vike-backend`, and `vike_agent_eval::cases`' `RUN_A_BACKTEST_LOCALLY` now grades an answer on
/// naming it.
///
/// Naming it put the joined text at **2063** against the ceiling (the gate's own parse). The bytes
/// came from two places that carried no fact: the record bullet's *— part of the data daemon now*
/// (a note from the day ruling 10 merged the recorder in, which the command itself now says) and
/// the opening's closing clause (see [`INSTRUCTIONS_OPENING`]). `key-less` did not fit, and it is
/// not lost: the refusal an operator meets on `--store`,
/// `vike_datahub_client::flag_vocab::store_flag_removed`, says it in the same breath as the command.
const INSTRUCTIONS_ELSEWHERE: &str = "\
When a request needs another part, NAME THE COMMAND rather than refusing blind — none is a tool \
you can call; a human runs them:
- record a live tape: `vike-backend datahub --record <profile>`.
- get bars in: `vike-cli data hist fetch binance:BTCUSDT:1h --days 180` (needs a datahub), or \
`vike-cli data hist fetch --source demo` for a synthetic tape.
- backtest on this box: `vike-cli backtest run --local --profile run.toml`. It still reads a \
datahub; for files here: `VIKE_DATAHUB_STORE=DIR vike-backend datahub`.
- which venue API keys this box holds and where from: `vike-cli secrets list` / \
`vike-cli secrets path`. One store: `settings/secrets.env` or a DB.";

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
/// ⚠ The last sentence is the OWNER's rule (`docs/decisions/0086` point 6), and it is an
/// instruction rather than a gate because nothing on this side can see the chat it is about: a
/// settings write changes a live setting on the node, so an agent makes one only after the owner
/// said yes. It REPLACED a sentence that told an agent to obtain the operator's RETYPING of the key
/// as `policy_confirm` — an argument deleted with the retype itself (point 7). Where a session HAS
/// no chat, the rule cannot be kept by asking, and `--unattended` holds it structurally instead
/// ([`unattended_refusal`]).
const INSTRUCTIONS_LIFECYCLE: &str = "\
`mount_strategy` / `unmount_strategy` / `set_setting` change the node's CONFIGURATION, not its \
book. A `set_setting` changes a LIVE setting on the node: make one only after the owner said yes \
in chat.";

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
    /// **The node's ACCOUNT-SET digest as it stood when this preview was taken.**
    ///
    /// A confirm compares the node's CURRENT value against this one and refuses on a difference:
    /// a preview describes a routing decision, and a routing decision made against a set that has
    /// since changed describes a node that no longer exists. `Some(0)` when the node published
    /// none — an older node, or one with no accounts — and two `Some(0)`s compare equal, which is
    /// the pre-field behaviour exactly.
    ///
    /// ⚠ **`None` means the node could not be ASKED, and it is a different fact from any answer
    /// it could have given.** This was a `u64` that folded a failed read into `0`, and the fold
    /// was not merely imprecise — it made the guard's verdict depend on WHEN visibility was lost
    /// rather than on whether the set moved. See [`Server::node_accounts_epoch`].
    accounts_epoch: Option<u64>,
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
    fn issue(&mut self, intent: PreviewIntent, accounts_epoch: Option<u64>) -> String {
        self.by_token.retain(|_, p| p.issued.elapsed() <= PREVIEW_WINDOW);
        self.next += 1;
        let token = format!("pv-{}", self.next);
        self.by_token.insert(
            token.clone(),
            PendingPreview { intent, issued: Instant::now(), accounts_epoch },
        );
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

/// Parse `--addr <datahub>` (default [`DEFAULT_ADDR`]), the optional `--node <host:port>` (the
/// vike-tradehub node the trade tools control), the tool SCOPE (`--profile`, repeatable
/// `--deny-tool`), the transcript request (`--trace` / `--trace-dir`) and `--unattended` (no person
/// attends this session — see [`unattended_refusal`]), via the shared
/// [`crate::cmd::args`] glue. A `--help`/`-h` short-circuits through [`args::help_requested`];
/// [`args::exit_for_parse_error`] in [`run`] is what turns that back into a stdout usage and an
/// exit 0.
///
/// ⚠ Both scope flags REFUSE a name they do not recognise rather than ignoring it, and that is the
/// same rule twice: a mistyped `--profile` must not serve more than the operator asked for, and a
/// mistyped `--deny-tool` must not leave them believing a tool was withheld when nothing was.
/// `--trace` and `--trace-dir` are last-one-wins, the ordinary CLI expectation for two spellings of
/// one destination.
fn parse_config(
    args: impl Iterator<Item = String>,
    configured_backtest_addr: Option<&str>,
    configured_datahub_addr: Option<&str>,
) -> Result<Config, String> {
    // ⚠ THE MIDDLE RUNG on the DATA plane, and it was missing for exactly as long as its
    // compute twin below was. `crates/vike-desktop/src/app_methods.rs` read `config.datahub_addr`
    // and was its ONLY reader, so a box whose datahub is not on the compiled-in default reached it
    // from the GUI and dialled `127.0.0.1:7878` from here — one setting, two clients, two answers.
    // A BLANK rung is skipped rather than honoured, same as the compute side: an `Environment=`
    // line that set nothing must not aim this at an empty address.
    let mut addr = configured_datahub_addr
        .filter(|s| !s.trim().is_empty())
        .map_or_else(|| DEFAULT_ADDR.to_string(), str::to_string);
    // ⚠ THE MIDDLE RUNG, and its absence was a recorded residual of the backtest-CLI-surface
    // design (§18 row 9): the dispatcher handed `config.backtest_addr` to `backtest` and
    // `research` and NOT here, so a box that set the key moved some of its compute dialers and
    // silently left this one on the compiled-in default. The ladder is now the same three rungs
    // every other compute dialer has — `--backtest-addr` → the setting →
    // `vike_config::DEFAULT_BACKTEST_ADDR`.
    let mut backtest_addr = configured_backtest_addr
        .filter(|s| !s.trim().is_empty())
        .map_or_else(|| vike_config::DEFAULT_BACKTEST_ADDR.to_string(), str::to_string);
    let mut node: Option<String> = None;
    let mut profile = Profile::Full;
    let mut denied: Vec<String> = Vec::new();
    let mut trace = TraceRequest::Off;
    let mut unattended = false;
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
            // A BOOLEAN, and it takes no value: `--unattended=false` would be a way to write the
            // flag and mean its opposite, so a value is refused like every other valueless flag.
            "--unattended" => {
                args::no_value(&flag, inline)?;
                unattended = true;
            }
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
        unattended,
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
    /// `--unattended` — see [`Server`]'s field of the same name.
    unattended: bool,
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
        let call = trace::Call {
            tool: name,
            write: is_write_tool(name),
            profile: self.access.profile_name(),
            args,
            verdict,
            detail,
            token,
            token_role,
        };
        if let Err(e) = trace.append(vike_model::now_ms(), &call) {
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
            // A placeholder rather than a phrase, because it is spliced into a `policy.venues` row
            // KEY below as well as into prose — `policy.venues.the venue` would read as a real key.
            let venue = arg("venue").unwrap_or("<venue>");
            Ok(format!(
                "You have been asked about arming {venue} for live trading. Read this before \
                 doing anything.

⚠ THIS AGENT SURFACE CANNOT ARM A VENUE, and that is deliberate. Arming is a settings-database \
                 write and a credential decision a human makes; there is no tool here that does it, \
                 and there should not be.

What actually decides, in the order the mount consults it:

1. `policy.venues.{venue}` — `paper` | `demo` | `live`, defaulting to `paper` for every venue, \
                 written with `vike-cli config set policy.venues.{venue} <mode>`. It is a CEILING \
                 and can only ever REFUSE: `live` arms nothing without the venue's credentials \
                 behind it. ⚠ But for binance, bybit, okx and hyperliquid `live` also means \
                 MAINNET (decision 0095), so with that venue's LIVE keys in the store it is real \
                 money: a deliberate choice, never a default. A box with no `policy.venues` rows \
                 mounts ALL PAPER whatever its credential store holds, and says so once at \
                 startup.
2. The credentials, read only if the ceiling allowed it. Absent credentials ARE the live gate — \
                 the venue stays on the paper simulator. `vike-cli secrets path` prints which \
                 store this project resolves to and `vike-cli secrets list` prints the key NAMES \
                 in it (never the values).

So the human's job is: raise the ceiling for that one venue with `vike-cli config set`, and put \
                 the right key set in the store. Yours is to tell them which of the two is missing.

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

#[path = "mcp_tests.rs"]
#[cfg(test)]
mod mcp_tests;
