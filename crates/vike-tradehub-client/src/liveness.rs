//! `liveness` — the ONE home for the node protocol's LINK-LIVENESS facts: how long a connection may
//! be quiet before somebody concludes the link is dead, and how a quiet link proves it is not.
//!
//! These are PROTOCOL FACTS, not operator knobs. Both ends must agree on them (a node that
//! heartbeats every 15 s and a client that gives up after 10 would tear a healthy link down every
//! quarter minute), so they live in ONE crate — this one, the LOWER of the two — and
//! `vike-tradehub`'s server CONSUMES them. Never the reverse and never a second copy: the layer
//! rule (root `CLAUDE.md`'s "Workspace layers (dependency direction: down only)" — "when two sides
//! must not disagree, the cure is a shared crate BELOW both", machine-checked by
//! `crates/vike-ops/tests/layer_gate.rs`) exists because a duplicated timeout drifts silently and
//! the symptom is a link dying on a timer nobody remembers setting. Deliberately NOT settings keys
//! either: an operator who could raise [`OBSERVE_READ_TIMEOUT`] on one side alone would be
//! configuring exactly that failure.
//!
//! ⚠ That citation used to name a section of the root page that does not exist: it quoted the
//! sentence about a shared constant living in the client crate as if it were a HEADING, when it is
//! the implementing brief's wording for the rule, not a title in that file.
//! `crates/vike-ops/tests/citation_gate.rs`'s `every_heading_citation_names_a_live_section` is the
//! gate that says so, it is a merge gate, and it was red on this branch — a section citation must
//! name a title the cited page actually carries.
//!
//! # The four questions these answer, and the defects behind each
//!
//! **1. What does an AUTHENTICATED connection experience when it is idle?** —
//! [`AUTHED_IDLE_TIMEOUT`]. Nothing, now. It used to be killed after five minutes:
//! `vike-tradehub`'s `server::handle_connection` set its handshake read timeout on the accepted
//! socket to bound an UNAUTHENTICATED peer and never replaced it after `AuthOk`, so a control
//! connection an agent holds for the life of its process was closed the first time that agent
//! thought for five minutes — and the client, which learns of a close only when it next writes,
//! reported the next command's outcome as UNKNOWN ("may have executed") for a command the node had
//! stopped reading before it was written.
//!
//! **2. How does a SILENT link prove it is alive?** — [`OBSERVE_HEARTBEAT`] and
//! [`OBSERVE_READ_TIMEOUT`]. A subscribed observe stream is one-way: the client sends nothing
//! after its `Subscribe`, and an idle node publishes nothing, so a link that dies WITHOUT a FIN or
//! RST (a laptop sleeping, a Wi-Fi change, a VPN re-key, with the local `ssh -L` holding its
//! forwarded socket open) was indistinguishable from a quiet one — FOREVER. `vike-cli mcp`'s
//! `node_snapshot` answered the pre-drop frame as live that whole time.
//!
//! ⚠ These paragraphs used to end that sentence "for hours, until the OS's two-hour TCP
//! keepalive", and so did three sibling docs. **That backstop does not exist in this workspace.**
//! TCP keepalive is off unless a socket asks for `SO_KEEPALIVE`, `std::net::TcpStream` cannot ask
//! (it is `socket2`'s surface, and this workspace refuses a dependency for one option), and
//! nothing here asks — `grep -rn keepalive crates/vike-tradehub crates/vike-tradehub-client`
//! returns prose and no call. A read parked on a silently-dead socket with no deadline is parked
//! for the life of the process. The wrong number mattered because it made the residual sound
//! bounded; see [`AUTHED_IDLE_TIMEOUT`]'s cost paragraph for the one place it still bites.
//!
//! **3. How long may a CONTROL command's reply take?** — [`CONTROL_REPLY_TIMEOUT`]. The write half
//! has the same silent-death exposure as the read half and did not have the same answer: the
//! worker thread wrote its `Request::Command` into a kernel send buffer that accepted it, then
//! blocked in an undeadlined reply read that nothing could ever end.
//!
//! **4. How long may a HANDSHAKE take?** — [`HANDSHAKE_REPLY_TIMEOUT`]. A half-open tunnel accepts
//! the local `TcpStream::connect` (the ssh client's forwarded port is a LOCAL socket; it accepts
//! long after the far end is gone) and then never answers, and the client's `Welcome` read blocked
//! forever — in `vike-cli mcp`'s case blocking the single-threaded MCP server with it.

use std::time::Duration;

/// The idle read policy an AUTHENTICATED connection gets, REPLACING the server's unauthenticated
/// handshake bound the moment `AuthOk` is written. `None` = no read timeout: an authed connection
/// may stay quiet indefinitely and the node will not close it.
///
/// It is an `Option<Duration>` rather than a deleted line precisely so the server's spelling is
/// `stream.set_read_timeout(AUTHED_IDLE_TIMEOUT)` — a REPLACEMENT that cannot be forgotten and that
/// both ends read from one place the day it becomes finite.
///
/// # Why "no timeout" is the right answer here, and not merely a bigger number
///
/// The bound this replaces exists to stop an UNAUTHENTICATED peer from pinning a connection thread
/// by opening a socket and saying nothing (`crates/vike-tradehub/src/server.rs`'s
/// `HANDSHAKE_READ_TIMEOUT` and `MAX_CONNECTIONS` both argue it in those terms). After `AuthOk` the
/// peer has proved possession
/// of a scoped key: the thread it holds is one it is entitled to hold, and the reachability barrier
/// the whole design rests on (loopback + an SSH tunnel — that module's "Reachability is the OUTER
/// barrier") is unchanged by how long it stays quiet.
///
/// A FINITE value cannot be made safe by size. A control client parks in its command channel, not
/// in a socket read, so it cannot know it must speak; keeping a link alive under a finite server
/// timeout would need a client-side timer sending pings — a background thread on a surface whose
/// whole safety story is that it has none — and any value chosen instead just makes the same
/// silent kill rarer and harder to attribute. Five minutes killed the connection an agent held
/// across one long think; an hour would kill it across lunch and be diagnosed a year later.
///
/// The node already behaves this way for the OTHER authed shape: after `Subscribe` the connection
/// thread stops reading entirely and becomes a writer, so a subscribed observer has had no read
/// timeout since PR-11. This makes the request/response half agree with the push half instead of
/// dying on a bound written for strangers.
///
/// # ⚠ What it costs — an UNBOUNDED node-side leak, and the number that used to hide it
///
/// This paragraph read "pins its node thread until the OS gives up (the two-hour TCP keepalive
/// default)". **CORRECTED:** there is no such backstop here. Keepalive is off on a socket that has
/// not set `SO_KEEPALIVE`, `std::net::TcpStream` has no way to set it, and nothing in this
/// workspace sets it (the module doc above carries the grep). So the honest statement is:
///
/// **A silently-dead AUTHED connection that is not a SUBSCRIBED observer pins its node connection
/// thread, and its `MAX_CONNECTIONS` reservation, FOREVER** — the thread parks in
/// `crates/vike-tradehub/src/server.rs`'s `handle_connection` read, and that function's `ConnSlot`
/// guard is released only when the thread ends. Each laptop-sleep / VPN-re-key cycle leaks one.
/// `MAX_CONNECTIONS` does NOT bound the leak; it bounds how many leaks it takes to wedge the node,
/// after which the accept loop refuses EVERY new connection — including the operator's — to a
/// daemon that is still trading, until it is restarted. Before this constant existed the 300 s
/// handshake bound reaped these within five minutes *by accident*, as a side effect of the defect
/// this branch removed; that accident was the only garbage collector the node had.
///
/// Two halves are NOT exposed, and saying which is the point: a SUBSCRIBED observer is fine
/// ([`OBSERVE_HEARTBEAT`] makes the node WRITE to it, so its thread dies on a failed write), and an
/// unauthenticated peer is fine (its bound is untouched). What leaks is a CONTROL connection, and
/// an authed observe peer that never sends `Subscribe`.
///
/// # Why the leak is ACCEPTED here rather than closed, and what reopens it
///
/// Both available cures were weighed and both are worse than the leak at this size:
///
/// * **`SO_KEEPALIVE` on the accepted socket** would close it exactly, and is unreachable: it needs
///   `socket2`, and the root `CLAUDE.md` transport rule refuses a new dependency for one option.
///   This is the cure to buy the day a second reason for `socket2` appears.
/// * **A finite-but-long value here** (hours, purely as a reaper) is refused because it is not a
///   reaper to every consumer. `vike-cli mcp`'s `Server::execute` recovers from a node-initiated
///   close ON THE SAME CALL — a `NeverSent` reconnects and sends — so for THAT consumer a finite
///   value would be nearly free. It is not free for the other two: `vike-app`'s order buttons are
///   fire-and-forget (`try_command`'s ticket is `let _ =`'d by contract), so a reaped link turns
///   the first click after a quiet night into a `NeverSent` NOBODY READS — a market-exit the
///   operator watched themselves press and that never went. Reaping a link an operator is about to
///   use, on a timer, to bound a resource leak nobody has hit, trades a silent capacity failure
///   for a silent ORDER failure. The condition that reopens this: `vike-app`'s control path growing
///   the same outcome-aware send `vike-cli mcp` has, or one observed wedge on a real node.
///
/// The client-side twin of this exposure is NOT accepted and is closed —
/// [`CONTROL_REPLY_TIMEOUT`]. It is the half that can lie to an agent rather than merely leak.
pub const AUTHED_IDLE_TIMEOUT: Option<Duration> = None;

/// How often the node re-asserts liveness on a SUBSCRIBED observe stream that has nothing to say —
/// the cadence of the `Response::Pong` the server writes when no snapshot frame was sent in this
/// long (`crates/vike-tradehub/src/server.rs`'s `run_push_writer`).
///
/// 15 s is chosen against the two numbers that bracket it. Below it sits the publisher's own poll
/// (`crates/vike-tradehub/src/publish.rs`'s `POLL_INTERVAL`, 15 ms) and the core's ≥16 ms coalesced publish cadence: a BUSY
/// node never emits a heartbeat at all, because it is already writing frames far faster than this,
/// so the cadence costs nothing where cost would matter. Above it sits the thing being detected —
/// a human or an agent noticing that a read is stale — where the unit is tens of seconds, and
/// [`OBSERVE_READ_TIMEOUT`] (three of these) is what that detection actually costs. An IDLE node
/// pays four ~30-byte frames a minute per subscriber, which is not a number anything in this
/// system can feel.
///
/// It is deliberately NOT tied to `POLL_INTERVAL`: that one is about not missing a publish, this
/// one is about not being mistaken for a corpse.
pub const OBSERVE_HEARTBEAT: Duration = Duration::from_secs(15);

/// How long a client's subscribed observe stream may be silent before its receive loop treats the
/// link as dead — a read timeout on the socket, so the EXISTING error arm of that loop
/// (`crates/vike-tradehub-client/src/remote_handle.rs`'s `RemoteCoreHandle`, whose receive thread
/// ends on `Err(_) => break`) flips `is_connected` and every caller's existing liveness gate starts
/// working on a silent death. Nothing new decides anything; the read simply stops blocking forever.
///
/// THREE heartbeats, not one. A bare `> OBSERVE_HEARTBEAT` would tear down a healthy link on the
/// first scheduling hiccup — the node's writer thread is one of many on a box the merge queue
/// saturates (the recorded the CI box CPU-starvation incidents are exactly this), and a missed
/// heartbeat is evidence of load, not of death. Three consecutive misses is the same "count the
/// misses" shape `ssh -o ServerAliveCountMax=3` uses for the identical question, and it bounds a
/// silent death at 45 s — against the hours it takes today.
///
/// ⚠ ARMED ONLY when the node advertises [`crate::proto::FEATURE_OBSERVE_HEARTBEAT`]. A node that
/// does not heartbeat must not be read with a deadline: the client would tear the link down every
/// 45 s on an idle node and reconnect, which is a worse failure than the one being fixed. See that
/// constant for why this is a feature negotiation rather than a version bump.
pub const OBSERVE_READ_TIMEOUT: Duration = Duration::from_secs(3 * OBSERVE_HEARTBEAT.as_secs());

/// How long a CONTROL worker waits for the node's reply to the command it just wrote, before it
/// gives that command up and ends the worker. The write half's answer to the question
/// [`OBSERVE_READ_TIMEOUT`] answers for the read half.
///
/// # The defect: a silent death on the WRITE socket had no answer at all
///
/// `crates/vike-tradehub-client/src/remote_control.rs`'s worker is a strictly serial
/// `write_frame` → `read_frame` loop, and its pre-write probe (`link_is_dead`) can only see a
/// close the socket REPORTED — silence peeks `WouldBlock`, which is the correct verdict for the
/// ordinary quiet link and therefore cannot be the verdict for a dead one. So a tunnel that died
/// silently let the write succeed (into a kernel send buffer nothing will ever drain) and parked
/// the reply read forever. Nothing ended it: no FIN, no RST, no keepalive (see the module doc),
/// and `crate::handshake::node_handshake` had cleared [`HANDSHAKE_REPLY_TIMEOUT`] on the way out.
///
/// The consequence was worse than a hung thread, which is why this half is closed while the node's
/// mirror-image leak ([`AUTHED_IDLE_TIMEOUT`]) is an accepted residual. `is_connected` stayed
/// `true` forever (it is stored `false` only after the loop), so nothing reconnected; every later
/// command queued behind the wedged worker; and `vike-cli mcp` answered each of them
/// `{"sent": true, "outcome": "unknown", …"it may still execute"}` for a command that provably
/// never left the process — the exact false statement this branch exists to eliminate, made
/// permanent for the life of the MCP server.
///
/// # Why 30 s, and why the honest verdict on expiry is UNKNOWN rather than never-sent
///
/// Three times [`HANDSHAKE_REPLY_TIMEOUT`], for strictly more work: answering a `Command` means a
/// `ControlLimits` token, a `RiskGate` check and a lowering into the core's command channel, none
/// of which touches a venue, a disk, or the fold's locks — microseconds on loopback, one tunnel
/// round trip otherwise. As with the handshake the value is set by the WORST HONEST case (a
/// saturated box whose connection thread has not been scheduled), because being wrong in the short
/// direction tears down a healthy link to a node that is merely busy.
///
/// On expiry the in-flight command is [`crate::CommandOutcome::Disconnected`] — UNKNOWN, it may
/// have executed — and NOT never-sent: it was written, and `write_all` reports no progress, so the
/// asymmetry the module doc states ("a wrong `Disconnected` costs a `node_snapshot`; a wrong
/// `NeverSent` places an order twice") decides it. Everything still QUEUED behind it is
/// `NeverSent`, which is provable and is what lets `vike-cli mcp`'s `Server::execute` reconnect
/// and send those on the same call.
///
/// ⚠ It bounds the WEDGE, it does not erase it. For up to this long after a silent death, a
/// second command offered to the same handle is enqueued and reported "sent, outcome unknown" when
/// it is really still in the queue. Shrinking the window to fix that would mean reaping healthy
/// links to slow nodes, so the trade is deliberate: the window is finite and self-heals (the worker
/// exits, the queue closes, the next offer is `ControlRejected::Gone` → never-sent → reconnect),
/// where before it was permanent.
pub const CONTROL_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a client waits for each HANDSHAKE reply (`Welcome`, then `AuthOk`) before failing the
/// connect. Applies to the handshake ONLY: `crate::handshake::node_handshake` clears it before
/// handing the authed stream back, so each caller's own policy (the observe stream's
/// [`OBSERVE_READ_TIMEOUT`], the control worker's [`CONTROL_REPLY_TIMEOUT`]) is the one that
/// governs the session.
///
/// Ten seconds is ~1000x the answer time and still visibly finite to a human. What a node does
/// between reading `Hello` and writing `Welcome` is mint 32 CSPRNG bytes and serialize them: no
/// venue call, no disk, no lock on anything the fold touches, so a healthy answer is microseconds
/// on loopback and one tunnel round trip otherwise. The value is set by the WORST honest case (a
/// saturated build box answering a request whose thread has not been scheduled yet), not the
/// typical one, because the cost of being wrong in the short direction is a refused connection to
/// a node that is perfectly fine.
///
/// The failure it exists for is not slowness at all: a half-open tunnel accepts the LOCAL connect
/// and then delivers nothing, forever. Before this, `vike-cli mcp` — a single-threaded server —
/// blocked in that read and stopped answering the agent entirely.
pub const HANDSHAKE_REPLY_TIMEOUT: Duration = Duration::from_secs(10);

// Compile-time bounds, the `crates/vike-tradehub/src/server.rs`
// (`MAX_CONNECTIONS`) / `crates/vike-tradehub/src/telegram/confirm.rs` idiom: a RANGE, so a
// deliberate tweak stays free while the ways these stop being a liveness contract at all do not
// compile.
const _: () = assert!(
    OBSERVE_READ_TIMEOUT.as_secs() >= 2 * OBSERVE_HEARTBEAT.as_secs(),
    "OBSERVE_READ_TIMEOUT must allow at least TWO missed heartbeats — one missed beat is evidence \
     of load, not of a dead link, and a client that tears a healthy stream down on it is worse \
     than the silent death it was written to catch"
);
const _: () = assert!(
    OBSERVE_HEARTBEAT.as_secs() > 0 && OBSERVE_HEARTBEAT.as_secs() <= 60,
    "OBSERVE_HEARTBEAT must stay a POSITIVE cadence a stale read can be noticed within — a zero \
     would make the node's writer thread a spin loop, and a minutes-scale value would put the \
     detection window (three of these) past the point where a human has already acted on the frame"
);
const _: () = assert!(
    HANDSHAKE_REPLY_TIMEOUT.as_secs() > 0,
    "HANDSHAKE_REPLY_TIMEOUT must stay POSITIVE — zero is not 'no timeout' to the socket layer, it \
     is a read that can never succeed"
);
const _: () = assert!(
    CONTROL_REPLY_TIMEOUT.as_secs() >= HANDSHAKE_REPLY_TIMEOUT.as_secs(),
    "CONTROL_REPLY_TIMEOUT must stay POSITIVE and no tighter than the HANDSHAKE reply it follows — \
     zero is a read that can never succeed, and a command reply is strictly MORE node work than \
     minting a nonce, so a value under the handshake's would reap healthy links to a busy node and \
     report every command it reaped as UNKNOWN"
);
