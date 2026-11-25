//! `vike-mcp` — a LOCAL stdio MCP server exposing the vike agent surface as tools, so Claude (or
//! any MCP client) can run the full loop: CREATE + BACKTEST (author a Rhai strategy, discover its
//! knobs, see the host-bound indicator set, run a backtest) AND — against a running vike-tradehub
//! node — read live state and place orders through a mandatory PREVIEW gate. Part of the "vike-cli
//! as the one agent surface" program
//! (`docs/superpowers/specs/2026-07-26-vike-cli-agent-surface-program.md`).
//!
//! # Transport: the official SDK, over a hand-written wire
//!
//! MCP's stdio transport is JSON-RPC 2.0 framed as NEWLINE-delimited JSON (one message per line, no
//! embedded newlines). The protocol is `rmcp`'s, used by this crate alone (`rmcp_seam.rs`, a
//! `ServerHandler` by hand, no `#[tool]`), and the lines are ours (`rmcp_wire.rs`), because the SDK's
//! own stdio loop loses answers a client is owed. Requests carry an `id` and get exactly one
//! response — unless the client CANCELS one with `notifications/cancelled`, which a daemon run
//! honours and which then gets none (see [`serve`]); notifications (no `id`) get none.
//!
//! # Tools
//!
//! READ-ONLY / safe (`readOnlyHint`): `validate_strategy`, `discover_params`, `list_templates`,
//! `list_indicators` (all OFFLINE — pure vike-rhai, no server), `profile_template` (OFFLINE too:
//! the compiled-in profile schema), `run_backtest`, `run_sweep`,
//! `run_walk_forward` (remote COMPUTE-daemon runs — a backtest MUTATES nothing),
//! `list_strategies` (the same daemon), `list_series` (datahub metadata), `node_snapshot`,
//! `strategy_status`, `settings_show`, `health_node`, `report`
//! (the PER-CALL node reads — see [`Server::tool_strategy_status`] for why they open their own
//! short-lived connection rather than riding the held observe pipe), `decode_order_id` (a client order
//! id in words, with the node's names when one answers). This is the ABSORBED `vike-mcp` tool
//! surface (Phase A of retiring that crate): the run/list tools go over a daemon's RPC verbs
//! instead of a local DataFusion store, so this CLI stays DataFusion-free; `validate_strategy` /
//! `list_templates` are offline ports (note the argument is named `script` here — vike-mcp said
//! `code` — matching this file's existing `discover_params`).
//!
//! ⚠ **This paragraph called all four run/list tools "remote datahub", and for the run verbs that
//! is not merely imprecise — it names a daemon that REFUSES them.**
//! `crates/vike-datahub/src/server.rs` answers `Request::RunParamscan` with
//! the plane refusal that names `RunParamscan`. Those four dial [`Server::backtest_addr`], whose own doc
//! records ruling 7 and argues why it is a SECOND field rather than a second meaning for
//! `datahub_addr`. Two tools DO dial the data server — `list_series` and `delete_series`, through
//! [`Server::datahub_for_delete`] — so the plane is a property of each TOOL and a paragraph that
//! names one daemon for all six can only be half right.
//!
//! LIVE WRITE (`destructiveHint`), gated — two families on ONE roster ([`WRITE_TOOLS`]) because
//! what the roster BUYS is the same for both: the mandatory preview gate, the `destructiveHint`,
//! the `read-only` withholding and the transcript's write classification.
//!
//!   * the ORDER verbs — `submit_order`, `submit_order_list`, `submit_combo`, `cancel_order`,
//!     `modify_order`, `flatten`, `close_all`, `set_trading_state`, `mass_cancel`;
//!   * the NODE-LIFECYCLE verbs ([`LIFECYCLE_TOOLS`]) — `mount_strategy`, `unmount_strategy`,
//!     `set_setting` — which change what the node RUNS and how it is CONFIGURED rather than what
//!     is in its book.
//!
//! **Both families are built by the SAME [`vike_client::verbs`] construction site the `trade` REPL
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
//! as where settings are changed ([`unattended_refusal`]). `crates/vike-agent-evaluation/src/unattended.rs`'s `mcp_argv`
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
//! the core `RiskGate` on the vike-tradehub are the ENFORCING backstop regardless — the client
//! preview is UX, the node gate is truth. Writes go to a running node named by `--node <addr>`,
//! signed with THIS box's device key (a `device_key` row of its settings database, see
//! [`vike_client::hubkeys`]), and the node's access list decides what that key reaches (`control`
//! for a write, `observe` for a read). No variable in the MCP server's environment carries a key (a
//! process that has an old hub-key variable set is refused at startup, `vike_config::REMOVED_ENV`);
//! with no device key, the trade tools return a clean error naming the store and the
//! create+backtest tools still work.
//!
//! **A venue the node does not mount, and an order over this box's notional ceiling, are REFUSED at
//! the preview, before a token is minted, and again at the send.** The gate is
//! `vike_client::order_gate::judge`, the one rule `vike-cli` asks too, behind
//! [`Server::vet_commanded_venue`], called from the router's `n if is_write_tool(n)` arm, so a new
//! write tool routed THROUGH that arm inherits it rather than needing a copy;
//! `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md` carries the disposition.
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
//! `crates/vike-mcp/src/node_drop_tests/drops.rs`'s
//! `the_venue_gate_reads_a_real_nodes_mounted_set_and_refuses_what_is_not_in_it`, and it is the
//! half that holds the EVIDENCE — `Server::mounted_venues`'s projection of the node's own frame,
//! whose failure mode is a silent fail-OPEN — rather than the decision.
//!
//! **Every `submit_order` carries an order client id.** `order_client_id` stays an OPTIONAL argument, but the
//! node REFUSES a remote submit with an empty one, so when the agent omits it this server MINTS one
//! ([`verbs::fill_order_client_id`], the same generator and wire form the live core uses, every id
//! opening `M`, this server's source letter) before the preview renders. The preview therefore
//! shows a real id, and the confirming call SENDS THAT id:
//! [`Server::call_tool`] executes the STORED preview (`previewed.cmd`), not the command it rebuilt
//! from the confirming arguments, so the agent never needs to pass the id back — [`same_intent`]
//! ignores the minted id precisely so a confirm without it still matches. `order_client_id` in the
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
//!     process opens no socket". A tool whose row declares `offline_too` (it dials only to add to
//!     an answer its arguments give: `decode_order_id`) is served too, reading no node, and says
//!     `openWorldHint: false` in that session's `tools/list`.
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
//! `--trace` (or `--trace-dir <dir>`) turns on [`crate::trace`]: one appended JSONL record
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
//!   thread is still alive (`TradehubCoreHandle::is_connected`). A dead one is dropped and ONE fresh
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
//!   `Response::Pong` every `liveness::OBSERVE_HEARTBEAT` and `TradehubCoreHandle` deadlines its read
//!   at three of them, so a silent death ends the receive loop through its EXISTING error arm and
//!   the second pass below reconnects transparently (the publisher hands its last frame on
//!   subscribe).
//! * **The control worker's reply read** (every write tool call): deadlined at
//!   `liveness::CONTROL_REPLY_TIMEOUT`. `TradehubControlHandle`'s pre-write probe cannot see a silent
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
//! silence unless this box's `preferences.observe_stall_timeout_ms` row raises it (a flaky link
//! that tears the stream down repeatedly), and it is armed only against a node that advertises the
//! capability (a client that deadlined a heartbeat-less node would tear down every healthy idle
//! stream). The control window (`preferences.control_reply_timeout_ms`, 30 s unless raised, via
//! `HubKeyring::patience`) BOUNDS the wedge rather than erasing it: for up to that window after a
//! silent death,
//! a second write tool call is enqueued and answered OUTCOME UNKNOWN while it is really
//! still in the queue — finite and self-healing now, permanent before. A per-call verb's reply read
//! (`preview_command` and its siblings) is deadlined too, at `liveness::ONE_SHOT_REPLY_TIMEOUT`, so
//! a link that dies inside that window blocks this server for at most that long. Running the
//! tunnel as `ssh -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -L …` still tears the
//! forwarded socket down on the ssh client's own schedule, which is why the setup page keeps that
//! line. The frame
//! carries no node-side timestamp, so there is still nothing here to AGE a frame by — the gate is
//! liveness, not freshness.

// The server's opt-in agent transcript (`--trace`).
pub(crate) mod trace;

// The tool families (code-layout phase 2, task 9). Each child holds one family's `impl Server`
// block and its free helpers, and `Server::call_tool` below routes to them by name; the roster
// they all hang off (`tools_spec`) has its own file.
mod background_runs;
mod backtest_tools;
mod data_tools;
mod node_reads;
mod node_writes;
mod offline_tools;
mod tool_args;
mod tool_schemas;
pub(crate) mod tool_verbs;
mod venue_gate;

mod cancel;
mod config;
mod instructions;
mod preview;
mod prompts;
mod protocol;
pub mod refusals;
mod resources;
mod rmcp_seam;
mod rmcp_wire;
mod scope;

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use vike_model::orders::client_id::{OrderClientIdSource, names_a_strategy};
use vike_tradehub_client::{TradehubControlHandle, TradehubCoreHandle};

#[cfg(doc)]
use self::{
    instructions::INSTRUCTIONS_LIFECYCLE,
    node_writes::LIFECYCLE_TOOLS,
    prompts::two_call_gate,
    resources::{RESOURCE_TOOLS, resources_spec},
    tool_schemas::tools_spec,
};
use background_runs::BackgroundRuns;
use backtest_tools::{
    tool_list_strategies, tool_run_backtest, tool_run_paramscan, tool_run_walk_forward,
};
use cancel::CancelToken;
use config::{BIN, parse_config, resolve_trace};
use data_tools::tool_list_series;
use node_writes::{preview_of, unattended_refusal};
use offline_tools::{
    DecodeNode, decode_order_id_arg, tool_decode_order_id, tool_discover_params,
    tool_list_indicators, tool_list_templates, tool_profile_template, tool_validate_strategy,
};
use preview::{DeleteIntent, PendingPreviews, PreviewIntent, same_intent};
use protocol::DeferredOutcome;
use resources::render;
use rmcp_seam::{Answer, Guarded, Job, Seam, info_of};
use rmcp_wire::{EOF_DEADLINE, Wire};
use scope::{Profile, ToolAccess};
use tool_args::WriteGate;
use trace::McpTrace;
use vike_client::hubkeys::HubKeyring;
use vike_client::resolved::{
    Resolved, datahub_keyring, hub_keyring, install_user_indicators, seal_refusal_text,
};
use vike_client::verbs;

const SERVER_NAME: &str = "vike-mcp";
/// The name a client SHOWS for this server (`serverInfo.title`); `server.json`'s `title` is held equal to it.
const SERVER_TITLE: &str = "Vike Trader";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The MCP server state: the datahub and COMPUTE-daemon addresses (two fields, not one — see
/// [`Server::backtest_addr`]), the optional vike-tradehub address, and the two lazily-opened
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
/// `crates/vike-mcp/tests/shipped_mcp/mcp_transcript.rs` is an integration test, so it is a separate crate and
/// can only reach [`serve`] through a public type — but it must not be able to hand this server a
/// node address or a key, because the whole property it proves is that a write never leaves the
/// machine. [`test_server`] is the only constructor it gets.
pub struct Server {
    datahub_addr: String,
    /// The backtesthub's address — where the four `Run*`/`list_strategies` tools go since
    /// ruling 7 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` moved
    /// those verbs off the data server. A SECOND field rather than a second meaning for
    /// `datahub_addr`, because this server holds tools on BOTH planes at once: `list_series` still
    /// dials the datahub and `run_backtest` no longer can.
    backtest_addr: String,
    node_addr: Option<String>,
    control: Option<TradehubControlHandle>,
    observe: Option<TradehubCoreHandle>,
    /// The advisory preview caps, resolved once at startup — see [`verbs::guardrail_capabilities`]. The
    /// notional half is this machine's `policy.max_notional_per_order` ceiling (settings
    /// unification, Phase 5); the node's own `ControlLimits` is what actually enforces.
    caps: verbs::GuardrailCapabilities,
    /// How long a write tool waits for the node's verdict on the command it just sent before
    /// reporting the outcome as unknown — `config.node_ack_wait_ms`, resolved once by the
    /// dispatcher and handed to [`run`]. A DEADLINE, not a sleep: `await_outcome` returns the
    /// instant that command's reply lands, so a generous wait costs nothing when the node answers.
    /// An agent acts on this answer, and "unknown" is the expensive one.
    ///
    /// ⚠ **This server is single-threaded on the request path**, so while a write tool waits the
    /// whole server does, and the row's ceiling (the control client's own reply timeout) is how
    /// long that can be. Running out is "outcome NOT known" — the command may still execute —
    /// never a refusal, a cancel or a resend.
    ack_wait: Duration,
    /// What a node dial signs with: this box's device key, resolved ONCE by the program from its
    /// settings database's `device_key` row (decision 0288 item 8), never from the environment. See
    /// [`vike_client::hubkeys`].
    keys: HubKeyring,
    /// This server's order-client-id generator — the node REFUSES a `Submit` with an empty
    /// `order_client_id`, so a tool call that omits the optional argument must still arrive with
    /// one. Its ids open `M` (`OrderClientIdSource::Mcp`). See [`verbs::order_client_id_minter`].
    order_client_ids: verbs::OrderClientIdMinter,
    /// Previews issued and not yet confirmed. A write tool executes ONLY against a token from
    /// this store that is unexpired and bound to the SAME command — see [`PendingPreviews`].
    pending: PendingPreviews,
    /// Whatever `run_backtest` last returned IN THIS SESSION, rendered, so the
    /// `vike://backtest/last` resource has something to serve. **Per-process and deliberately so:**
    /// there is no report store in this workspace, and inventing one behind a resource read is a
    /// storage decision, not a protocol one. `None` until a backtest has run, and the resource says
    /// exactly that rather than serving an empty document.
    last_backtest: Option<String>,
    /// The runs started with `background: true`, which `run_status` and `cancel_run` read; per-process like
    /// `last_backtest`, and every one still running stops when this server is dropped ([`BackgroundRuns`]).
    background: BackgroundRuns,
    /// WHICH tools this server serves, resolved ONCE at startup from `--profile` and `--deny-tool`.
    /// The advertised roster and the routing gate are both read off this one value — see
    /// [`ToolAccess`].
    access: ToolAccess,
    /// The agent transcript, when `--trace`/`--trace-dir` asked for one. `None` is the default and
    /// means NOTHING is written and no directory is created — see [`crate::trace`].
    trace: Option<McpTrace>,
    /// The DATAHUB keys, for every tool that dials a data plane: this box's device key authenticates
    /// both the data daemon and the backtesthub (`vike_backtesthub_client::BacktesthubClient::connect_authed_on`'s
    /// doc).
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
    /// [`HubKeyring`] does for the tradehub: `vike_client::resolved`'s `datahub_keyring`, this box's device key
    /// alone (decision 0288 item 8: no shared datahub pair is read), threaded through [`run`].
    ///
    /// `None` remains the ordinary answer on a box with no device key, and it still reaches the
    /// unauthenticated constructor — so a key-less datahub behaves exactly as it did before.
    datahub_keys: Option<vike_hub_protocol::auth::HubKeys>,
    /// `--unattended`: nobody attends this session, so no setting may be changed through it — the
    /// owner's rulings on decision 0040 (2026-09-28 for `policy.*`, 2026-09-29 for every key).
    /// `false` (an ATTENDED session, the default) keeps `set_setting` for every key behind the
    /// ordinary preview gate.
    /// See [`unattended_refusal`].
    unattended: bool,
    /// How long [`serve`] waits for what is in flight once its input has ended: a run's reply read has
    /// no bound of its own, so a dead daemon link would otherwise leave this process for ever.
    /// [`rmcp_wire::EOF_DEADLINE`], and a field so that a test can wait less.
    eof_deadline: Duration,
}

/// A tool call that did not produce a result, and WHETHER A GATE SAID SO.
///
/// ⚠ The `refused` bit exists for the transcript: "the agent was refused" and "the tool broke" are
/// different facts, and a record that spelled them the same would be useless for the question it is
/// kept for. It is carried on the error rather than sniffed out of the message afterwards, because
/// a string match on prose is a gate a reworded sentence silently disarms.
///
/// `From<String>` is load-bearing exactly as `crates/vike-cli/src/exit.rs`'s `CliError`'s is: every tool helper in this
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

/// How long a metadata read of a hub (`list_series`, `list_strategies`) waits for its reply before it
/// calls the link dead, in place of the verb's default five minutes. Each is answered on the loop's
/// thread, so everything behind it waits with it, a `close_all` included, and a cancel is not read;
/// the answer itself takes about a second (`vike_hub_protocol::conn`'s `REPLY_IDLE_TIMEOUT`).
const HUB_READ_WAIT: Duration = Duration::from_secs(30);

/// A start up to its first flag, in the order a start has always had: refuse a stale environment and
/// load this box's settings (`vike_boot::boot`'s order, each departure a named arm with its reason), the
/// user's indicators, then the seal refusal. So `--help` under an unsound seal or a removed variable is
/// refused like every other start. `vars` is the root's one environment sweep and `walk_from` where the
/// project walk starts: `src/main.rs`'s working directory, or the folder the desktop's executable sits
/// in for `vike-desktop --mcp`. `Err` is the exit, its line already on stderr. No boot anchor is
/// written, and `crates/vike-boot/tests/one_owner/boot_journal_wiring.rs`'s row says why.
pub fn boot(vars: HashMap<String, String>, walk_from: Option<&Path>) -> Result<Resolved, ExitCode> {
    let booted = vike_boot::boot(&vike_boot::BootSpec {
        env: &vars,
        cwd: walk_from,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Refuse,
        // REPORT: `Resolved::seal_refusal` carries the mark, and the refusal below comes after the
        // user's indicators are installed, the order this server has always started in.
        settings: vike_boot::SettingsLoad::Load,
        credentials: vike_boot::Credentials::Deferred(
            "this box's device key is read by `hub_keyring` and `datahub_keyring` from its \
             `device_key` row, and no venue credential is opened at all: this server reaches none \
             (docs/decisions/0036).",
        ),
        log_home: vike_boot::LogHome::Elsewhere(
            "this server builds no log subscriber: its stdout is a protocol, and everything it has \
             to say goes to stderr.",
        ),
        disclosure: vike_boot::Disclosure::Skip(
            "`boot_lines` re-reads every settings file to recover each row's ORIGIN, a daemon's \
             one-off cost. `vike-cli config show` prints all of it, on request.",
        ),
    });
    let booted = match booted {
        Ok(booted) => booted,
        Err(e) => {
            eprintln!("{BIN}: {e}");
            return Err(ExitCode::FAILURE);
        }
    };
    let resolved = Resolved::from_booted(booted, vars, BIN);
    install_user_indicators(BIN, resolved.user_data_dir.as_deref());
    // This program ACTS on the ceilings, so it refuses where `vike-cli`'s repair verbs still run.
    // `crates/vike-cli/src/dispatch.rs`'s `SEAL_REFUSING_VERBS` is the same rule for `trade`.
    if let Some(why) = resolved.seal_refusal.as_deref() {
        eprintln!("{}", seal_refusal_text(BIN, why));
        return Err(ExitCode::FAILURE);
    }
    Ok(resolved)
}

/// The server over what [`boot`] resolved, for every root that serves it (`src/main.rs` and
/// `vike-desktop --mcp`): this box's two keyrings, then `argv` (the flags after the program's own
/// words) parsed, then the stdio MCP loop until EOF.
///
/// The boot's `policy_max_notional` is this machine's `max_notional_per_order` policy ceiling (it
/// replaced the removed `VIKE_MAX_ORDER_NOTIONAL`), used only by the mandatory preview's advisory
/// guardrail. `node_ack_wait` is `config.node_ack_wait_ms`: how long a write tool waits for the
/// node's verdict on its command. The keyring is this box's device key: the root owns the
/// settings-database read, this file takes the answer.
///
/// # ⚠ Why this verb has NO connect/refuse rung of its own
///
/// Every other verb that opens a socket classifies the failure onto `crates/vike-cli/src/exit.rs`'s ladder. This
/// one deliberately does not, and the reason is structural rather than an omission: a `mcp` process
/// is a SERVER, and its per-call failures are answers, not exits. Each of its
/// `DatahubClient::connect` / `TradehubControlHandle::connect` / `TradehubCoreHandle::connect` sites
/// returns the failure as a JSON-RPC tool error to the client that asked, which then decides what
/// to do — exiting the process on one would kill an agent's whole session over a single
/// unreachable node. The same goes for a refusal of the order gate (`vike_client::order_gate::judge`):
/// it is that call's `isError` answer, and no token is minted for it.
///
/// So this function has exactly two process outcomes, and both are already right. The parse error
/// goes through `vike_utility::cli`'s `CliExit::finish` and lands on the usage rung, the number
/// `vike-cli`'s verbs exit on — and so, deliberately, does the ONE startup refusal added since: a `--trace` that
/// cannot resolve a project directory (see [`resolve_trace`]). It is a request that cannot be
/// honoured, which is what the usage rung means; it is not a per-call failure, because no client
/// has connected yet. The only other failure is [`serve`]'s `io::Result` — a stdio read or write that
/// broke — which is the ordinary "it ran and failed" rung by definition, and is what
/// `ExitCode::FAILURE` already spells.
pub fn run(argv: impl Iterator<Item = String>, resolved: &Resolved) -> ExitCode {
    let keys = hub_keyring(resolved, BIN);
    let datahub_keys = datahub_keyring(resolved, BIN);
    // The node's middle rung rides the keyring (`config.node_addr`, `HubKeyring::node_addr`), `vike-cli trade`'s too.
    let config = match parse_config(
        argv,
        resolved.backtest_addr.as_deref(),
        resolved.datahub_addr.as_deref(),
        keys.node_addr(),
    ) {
        Ok(c) => c,
        Err(stop) => return stop.finish(),
    };
    let trace = match resolve_trace(config.trace, resolved.state_dir.as_deref()) {
        Ok(t) => t,
        Err(msg) => return vike_utility::cli::CliExit::refusal(BIN, msg).finish(),
    };
    if let Some(trace) = &trace {
        // Retention first, then SAY WHERE IT WENT — on stderr, because this process's stdout is the
        // JSON-RPC stream. An operator who asked for a record and is not told the path has to go
        // looking for it after the incident rather than before.
        trace.prune(trace::DEFAULT_MAX_TRACE_FILES);
        eprintln!("vike-mcp: recording the agent transcript under {}", trace.dir().display());
    }
    let mut server = Server {
        datahub_addr: config.datahub_addr,
        backtest_addr: config.backtest_addr,
        node_addr: config.node_addr,
        control: None,
        observe: None,
        caps: verbs::guardrail_capabilities(resolved.policy_max_notional),
        ack_wait: resolved.node_ack_wait,
        keys,
        order_client_ids: verbs::order_client_id_minter(OrderClientIdSource::Mcp),
        pending: PendingPreviews::default(),
        last_backtest: None,
        background: BackgroundRuns::new(),
        access: config.access,
        trace,
        // From the program's one device-key read — see the field, and `vike_client::resolved`'s
        // `datahub_keyring` for why this is the only place that read can happen.
        datahub_keys,
        unattended: config.unattended,
        eof_deadline: EOF_DEADLINE,
    };
    // stdin is read on `serve`'s reader thread, so it goes in as the `Send` handle (a `StdinLock`
    // holds a mutex guard and cannot cross threads); stdout stays LOCKED on this thread, the one
    // writer.
    let stdout = io::stdout();
    match serve(io::BufReader::new(io::stdin()), stdout.lock(), &mut server) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-mcp: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A [`Server`] with NO node address and NO credentials, for tests that drive the transport.
///
/// The absent node is what makes the harness safe AND what makes it prove something: a confirm
/// that clears the preview gate fails at the CONNECTION (`dial.node.unset`, no node to dial),
/// which is positive evidence it got PAST the gate — while a confirm the gate refuses never
/// reaches that error at all. The two outcomes are distinguishable, and neither one can send a
/// byte to a venue.
///
/// ⚠ Its datahub and backtesthub are port 1 (nothing binds it, so a dial is refused at once), never the
/// compiled defaults: the CI box runs the PRODUCTION hubs there beside the lanes these tests run on, and a
/// tool a test drives with no address of its own dialled them (`crates/vike-ops/tests/hygiene/hub_port_dial_gate.rs`).
///
/// ⚠ A second constructor, not a replacement for the inline `mod tests`' own `server()` helper —
/// that one is used by dozens of tests and stays. This one exists because `tests/` is a SEPARATE
/// CRATE and cannot build a struct whose fields are private.
pub fn test_server() -> Server {
    Server {
        datahub_addr: "127.0.0.1:1".to_string(),
        backtest_addr: "127.0.0.1:1".to_string(),
        node_addr: None,
        control: None,
        observe: None,
        caps: verbs::GuardrailCapabilities::default(),
        // The wait a box with no `config.node_ack_wait_ms` row runs — the row's own default, so a
        // test server waits what a production one does.
        ack_wait: Duration::from_millis(u64::from(vike_config::config::DEFAULT_NODE_ACK_WAIT_MS)),
        keys: HubKeyring::default(),
        order_client_ids: verbs::order_client_id_minter(OrderClientIdSource::Mcp),
        pending: PendingPreviews::default(),
        last_backtest: None,
        background: BackgroundRuns::new(),
        // The DEFAULT scope and NO transcript — so this constructor still describes exactly the
        // server every test that predates those two features was written against.
        access: ToolAccess::full(),
        trace: None,
        datahub_keys: None,
        // ATTENDED, like every server that predates the flag.
        unattended: false,
        eof_deadline: EOF_DEADLINE,
    }
}

/// A [`test_server`] scoped to one profile by NAME, for the transport tests in
/// `crates/vike-mcp/tests/shipped_mcp/mcp_transcript.rs` — a separate crate, which cannot name [`Profile`].
///
/// Panics on an unknown name rather than falling back, for the same reason [`Profile::parse`]
/// refuses one: a test that silently ran under `full` while claiming to prove `read-only` would be
/// the worst possible failure of this suite.
pub fn test_server_under(profile: &str) -> Server {
    let profile = Profile::parse(profile).expect("the test names a real profile");
    Server { access: ToolAccess::new(profile, Vec::new()), ..test_server() }
}

/// The stdio session, to EOF. Split from [`run`] so tests drive it, which is why it is `pub`: an
/// integration test is a separate crate.
///
/// # One loop owns the [`Server`], and the SDK runs beside it
///
/// `rmcp` lives on a thread of its own (`mcp-rmcp`, one single-threaded runtime) and reads its lines
/// from [`Wire`], which reads stdin on another (`mcp-stdin`). Each handler ([`Seam`]) sends this thread
/// a [`Job`], and this thread, the ONLY writer of `writer` (in production the locked stdout), is the
/// only one that touches the [`Server`]. So:
///
/// - every request is answered on this thread, EXCEPT the three [`protocol::DEFERRED_TOOLS`], which
///   run on a worker (`mcp-tool`) per call and come back as a [`Job::Finished`];
/// - a `notifications/cancelled` naming one of those runs closes its daemon socket and it gets no
///   response. A cancel naming an unknown, finished or inline id does nothing, a CONFIRMED WRITE
///   above all (`crates/vike-mcp/src/rmcp_wire.rs`'s module doc);
/// - at EOF this keeps answering until nothing is in flight, so input that ends while a run is in
///   flight still gets that run's response, for up to [`Server::eof_deadline`]. A worker that was
///   cancelled is not waited for, and neither is a `background: true` run: it was answered (its run
///   id), and it stops when the server is dropped (`background_runs`'s module doc).
pub fn serve(
    reader: impl BufRead + Send + 'static,
    mut writer: impl Write,
    server: &mut Server,
) -> io::Result<()> {
    let (jobs, inbox) = mpsc::channel::<Job>();
    let seam = Seam { jobs: jobs.clone(), info: info_of(server) };
    let wire = Wire::open(reader, jobs.clone(), server)?;
    std::thread::Builder::new().name("mcp-rmcp".into()).spawn(move || {
        let session = || -> io::Result<()> {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            runtime.block_on(async {
                let running = rmcp::service::serve_directly(Guarded(seam), wire, None);
                running.waiting().await.map(drop).map_err(io::Error::other)
            })
        };
        let _ = jobs.send(Job::Ended(session()));
    })?;
    for job in &inbox {
        match job {
            Job::Message { msg, reply } => {
                let answer = match server.classify(&msg) {
                    Some(call) => Answer::Deferred(Box::new(call)),
                    None => Answer::Inline(server.handle(&msg)),
                };
                let _ = reply.send(answer);
            }
            Job::Finished { call, outcome, reply } => {
                let response = match outcome {
                    DeferredOutcome::Finished(outcome) => {
                        Some(server.finish_deferred(*call, outcome))
                    }
                    DeferredOutcome::Cancelled => {
                        server.trace_cancelled(&call);
                        None
                    }
                };
                let _ = reply.send(response);
            }
            Job::Output(bytes) => {
                writer.write_all(&bytes)?;
                writer.flush()?;
            }
            Job::Ended(result) => return result,
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
        // A retired order client id key is REFUSED by name, never read as absent: an absent id is
        // minted, so a retried order would go out twice.
        if let Some(why) = vike_model::retired_order_client_id_key(args) {
            return Err(ToolError::refused(why));
        }
        match name {
            "validate_strategy" => Ok(tool_validate_strategy(args)?),
            "discover_params" => Ok(tool_discover_params(args)?),
            "list_templates" => Ok(tool_list_templates()),
            "list_indicators" => Ok(tool_list_indicators(args)?),
            "profile_template" => Ok(tool_profile_template(args)?),
            // `background: true`: answered at once with a run id, the run on a worker of its own (the
            // same `run_deferred` a foreground run goes through), read by `run_status`.
            run if protocol::DEFERRED_TOOLS.contains(&run) && protocol::in_the_background(args) => {
                let call = self.deferred_call(Value::Null, run, args.clone());
                self.background.start(call)
            }
            "run_status" => Ok(self.background.status(args)?),
            "cancel_run" => Ok(self.background.cancel(args)?),
            "run_backtest" => {
                let keys = self.datahub_keys.as_ref();
                let report =
                    tool_run_backtest(&self.backtest_addr, keys, args, &CancelToken::new())?;
                // Remember it so `vike://backtest/last` has something to serve — the resource is a
                // second WAY IN to this answer, not a second source of it. Recorded only on
                // success: a failed run must not replace the last report that actually completed.
                self.last_backtest = Some(render(&report));
                Ok(report)
            }
            "run_sweep" => {
                let keys = self.datahub_keys.as_ref();
                Ok(tool_run_paramscan(&self.backtest_addr, keys, args, &CancelToken::new())?)
            }
            "run_walk_forward" => {
                let keys = self.datahub_keys.as_ref();
                Ok(tool_run_walk_forward(&self.backtest_addr, keys, args, &CancelToken::new())?)
            }
            "list_strategies" => {
                let keys = self.datahub_keys.as_ref();
                Ok(tool_list_strategies(&self.backtest_addr, keys, HUB_READ_WAIT)?)
            }
            "list_series" => {
                let keys = self.datahub_keys.as_ref();
                Ok(tool_list_series(&self.datahub_addr, keys, HUB_READ_WAIT)?)
            }
            // ⚠ A NAMED arm ABOVE the `is_write_tool` guard, and both halves of that sentence are
            // load-bearing. It is in [`WRITE_TOOLS`] — so it inherits the `destructiveHint`, the
            // `read-only` withholding and the transcript's write classification by construction —
            // but it is NOT a node command: it removes stored history through a datahub, and
            // `tool_verbs`'s `wire_command_for` has no spelling for it and must not grow one.
            // `the_data_deleter_keeps_every_guard` is what holds the pairing.
            "delete_series" => self.tool_delete_series(args),
            "node_snapshot" => Ok(self.tool_node_snapshot()?),
            "strategy_status" => Ok(self.tool_strategy_status()?),
            "settings_show" => Ok(self.tool_settings_show()?),
            "health_node" => Ok(self.tool_health_node()?),
            "report" => Ok(self.tool_report(args)?),
            // The names come from the node when one answers; with none the id alone still decodes.
            "decode_order_id" => {
                let id = decode_order_id_arg(args)?;
                let node = self.decode_node(&id);
                Ok(tool_decode_order_id(&id, node))
            }
            // The write tools: build the CommandWire through the SHARED `vike_client::verbs`
            // construction site the trade REPL resolves through — all ten of them, orders and node
            // lifecycle alike — then gate on `confirm`. The guard reads [`WRITE_TOOLS`] rather than
            // re-spelling it as an alternative pattern, so this arm cannot drift from the roster
            // the annotations and the transcript harness read — see
            // `the_write_arm_and_the_write_roster_are_the_same_set`.
            n if is_write_tool(n) => {
                let cmd = tool_verbs::wire_command_for(name, args)?;
                // MINT the order-client-id when the agent did not supply one — the node REFUSES a
                // remote `Submit` with an empty `order_client_id`, so `submit_order` without the
                // optional argument used to be un-executable. Minted BEFORE the confirm branch so
                // the preview shows a real id; a `confirm: true` call mints its own (the preview
                // note says so, and `order_client_id` in the accepted response is authoritative).
                // The id takes its shape, account code and intent from the frame the venue gate
                // reads next, so a venue-less verb still costs no node round trip. No bracket comes
                // through here (`NOT_ON_THE_MCP`), so the link's `takes` is asked only if one did.
                let frame = cmd.addressed_venue().and_then(|_| self.order_gate_frame().ok());
                let cmd = verbs::fill_order_client_id(
                    cmd,
                    &mut self.order_client_ids,
                    frame.as_deref(),
                    |c| self.control.as_ref().is_some_and(|control| control.takes(c)),
                );
                // The optional rationale is read SEPARATELY (it rides beside the command, never
                // inside it) and carried straight through to the node's audit trail.
                let gate = WriteGate::of(args);
                let reason = gate.reason();
                // ── THE ORDER GATE ───────────────────────────────────────────────────────────
                // BEFORE the confirm branch, so it covers the PREVIEW and the CONFIRM with one
                // check and so a token is never minted for a command this box refuses: the one rule
                // `vike-cli` asks too (`vike_client::order_gate::judge`: the routing rule and this
                // box's notional ceiling). See [`Server::vet_commanded_venue`].
                let ending = if gate.confirmed_token().is_some() {
                    "Nothing was sent."
                } else {
                    venue_gate::NOTHING_PREVIEWED
                };
                let (guardrail, venue_check) = self.vet_commanded_venue(&cmd, ending)?;
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
                let Some(token) = gate.confirmed_token() else {
                    // MANDATORY PREVIEW: describe what WOULD happen, execute nothing — and ask the
                    // NODE what it would do, because the client-side guardrail cannot price a
                    // market order (no `price` to size) and market is the default order type.
                    let node = self.node_preview(&cmd);
                    // A settings write's preview also shows WHAT it changes — `old → new`, read
                    // off the node (`None` for every other command).
                    let change = self.setting_change(&cmd);
                    let frame = self.order_gate_frame();
                    let epoch = frame.as_ref().ok().map(|frame| frame.accounts_epoch);
                    // What the node has just refused has nothing to confirm: no token, so no second call
                    // can send it. A node that could not be asked refused nothing, and its preview keeps
                    // one (the node judges the send).
                    let issued =
                        (!node.as_ref().is_some_and(node_writes::refused_by_node)).then(|| {
                            self.pending.issue(PreviewIntent::Node(Box::new(cmd.clone())), epoch)
                        });
                    let mut preview = preview_of(
                        name,
                        &cmd,
                        reason.as_deref(),
                        &guardrail,
                        issued.as_deref(),
                        node,
                        venue_check,
                        change.as_ref(),
                        frame.as_deref().map_err(String::as_str),
                    );
                    // Its tier and contract value are read off that frame.
                    node_reads::stamp_frame_age(&mut preview, self.frame_age());
                    return Ok(preview);
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
                // intent (checked above) but only the stored one carries the order_client_id the
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
                // The gate once more, at the send, on the control link's real features: the node may
                // have been reached for the first time just now, and its frame read again.
                self.gate_at_the_send(previewed_cmd)?;
                let mut answered = self.execute(previewed_cmd, reason)?;
                // ...and the venue disposition rides the ANSWER too, not the preview alone. The
                // write that actually happened is the one whose record has to say whether its
                // venue was compared against the node's mounted set, because
                // [`VENUE_CHECK_UNVERIFIED`] is a real ALLOW path: a transcript showing only
                // `outcome: "accepted"` cannot tell an operator whether the gate ran. Stamped
                // here rather than inside [`Server::execute`] so the one site that COMPUTES the
                // verdict is the one site that reports it. An OUTCOME UNKNOWN write is an `isError`
                // answer (`Server::offer_command`), which carries none.
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
            // that would not help. A name it served once IS refused, naming the new one
            // ([`refusals::RENAMED_TOOLS`]): answered as unknown, an agent would think it gone.
            other => Err(match refusals::renamed(other) {
                Some(renamed) => ToolError::refused(renamed.rendered()),
                None => ToolError::from(format!("unknown tool: {other}")),
            }),
        }
    }

    /// What the node gives `decode_order_id` for `id`: nothing under `offline` or with no `--node`;
    /// otherwise its frame, and its mount rows only when `id` names a strategy (one read otherwise).
    fn decode_node(&mut self, id: &str) -> DecodeNode {
        if self.access.reads_no_node() {
            return DecodeNode::Offline;
        }
        if self.node_addr.is_none() {
            return DecodeNode::NoNode;
        }
        let frame = match self.order_gate_frame() {
            Ok(frame) => frame,
            Err(why) => return DecodeNode::Unreachable(None, why),
        };
        if !names_a_strategy(id) {
            return DecodeNode::Read(frame, Vec::new());
        }
        let mounts = self.observe_read_target().and_then(|(addr, key)| {
            vike_tradehub_client::strategy_status(addr.as_str(), key)
                .map_err(|e| format!("strategy_status from {addr}: {e}"))
        });
        match mounts {
            Ok(status) => DecodeNode::Read(frame, status.mounts),
            Err(why) => DecodeNode::Unreachable(Some(frame), why),
        }
    }
}

/// The MCP write-tool roster — the FULL `trade`-REPL write-verb set (the drift the shared
/// [`vike_client::verbs`] module exists to prevent). Module scope rather than `#[cfg(test)]`
/// because three consumers now need it outside the inline test module: the [`Server::call_tool`]
/// routing, the registry manifest gate, and `crates/vike-mcp/tests/shipped_mcp/mcp_transcript.rs`.
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
/// every node command among them resolves through [`vike_client::verbs`].
/// ⚠ The LAST member is not a node command at all, and its membership is the whole of its gating.
/// `delete_series` removes stored history through a datahub; it resolves through NO
/// `vike_client::verbs` spelling and must not grow one. What being on this roster BUYS it is exactly
/// what it needs: the mandatory preview, the intent-bound token, the `destructiveHint`, the
/// `read-only` withholding and the transcript's write classification — four edges from one line.
/// Removing it from here would silently drop all four in one edit, which is the failure
/// `the_data_deleter_keeps_every_guard` exists for.
pub const WRITE_TOOLS: [&str; 13] = [
    "submit_order",
    "submit_order_list",
    "submit_combo",
    "cancel_order",
    "modify_order",
    "flatten",
    "close_all",
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

/// The workspace's refusal catalog, `refusals.json`, as this server serves it (`vike://refusals`): every refusal id,
/// its text template and the crate that declares it. RENDERED, never edited by hand: only `vike-backend` links every
/// crate that refuses, so `crates/vike-backend/tests/refusal_catalog.rs` renders it from each refusal enum's `ALL` and
/// holds this copy equal to its render. The release ships the same file beside `cli.json`.
pub const REFUSAL_CATALOG: &str = include_str!("refusals.json");

// The NODE-DROP suite — its own file because it stands up a real paper node behind a loopback
// relay the test owns (a harness, not one more case for the inline module below), and a child
// module rather than an integration test because it must reach `Server`'s private fields:
// `Server`'s doc argues that no PUBLIC constructor may take a node address or a key.
// ⚠ `#[path]` is what keeps the historic module name below. The src-walking gates find this file through the
// `#[cfg(test)]` on its declaration and resolve it the way rustc does, `#[path]` included
// (`vike_utility::libm_walk::cfg_test_module_rel_files`), so the module NAME no longer has to equal
// the file STEM. It used to: until 2026-10-05 three of those gates carried a copy of the resolver
// that read no `#[path]` and derived `{dir}/NAME.rs`, and this module was `node_drop_tests` for one
// commit and resolved a file that does not exist — leaving the real one classified as LIBRARY
// code. The name was kept rather than churned back.
#[path = "node_drop_tests.rs"]
#[cfg(test)]
mod mcp_node_drop_tests;

#[path = "tests.rs"]
#[cfg(test)]
mod mcp_tests;
