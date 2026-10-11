//! `liveness` — the ONE home for the node protocol's LINK-LIVENESS facts: how long a connection may
//! be quiet before somebody concludes the link is dead, and how a quiet link proves it is not.
//!
//! These are PROTOCOL FACTS, not operator knobs and not settings keys: both ends must agree (a node
//! that heartbeats every 15 s and a client that gives up after 10 would tear a healthy link down
//! every quarter minute), so they live in this crate, the LOWER of the two, and `vike-tradehub`'s
//! server CONSUMES them, never a second copy (root `CLAUDE.md`'s "Workspace layers (dependency
//! direction: down only)"). An operator who could raise [`OBSERVE_READ_TIMEOUT`] on one side alone
//! would be configuring exactly that failure.
//!
//! The four questions, and who consumes each answer:
//!
//! 1. What does an AUTHENTICATED idle connection experience? [`AUTHED_IDLE_TIMEOUT`] (the node's
//!    connection loop): nothing.
//! 2. How does a SILENT observe stream prove it is alive? [`OBSERVE_HEARTBEAT`] (the node's push
//!    writer) and [`OBSERVE_READ_TIMEOUT`] (`RemoteCoreHandle`'s receive loop).
//! 3. How long may a CONTROL command's reply take? [`CONTROL_REPLY_TIMEOUT`] (the
//!    `remote_control` worker).
//! 4. How long may a HANDSHAKE take? [`HANDSHAKE_REPLY_TIMEOUT`] (`crate::handshake`); and the
//!    directory read's reply, [`DIRECTORY_REPLY_TIMEOUT`] (`remote_handle::directory`).
//!
//! ⚠ **There is no TCP-keepalive backstop in this workspace.** Keepalive is off unless a socket sets
//! `SO_KEEPALIVE`, `std::net::TcpStream` cannot (it is `socket2`'s surface, and this workspace
//! refuses a dependency for one option), and nothing here does (`grep -rn keepalive` over both
//! tradehub crates finds prose and no call). A read parked on a silently-dead
//! socket (a sleeping laptop, a Wi-Fi change, a VPN re-key under an `ssh -L` that keeps its
//! forwarded socket open) with no deadline is parked for the life of the process.

use std::time::Duration;

/// The idle read policy an AUTHENTICATED connection gets, REPLACING the server's unauthenticated
/// handshake bound the moment `AuthOk` is written. `None` = no read timeout: an authed connection
/// may stay quiet indefinitely. An `Option` so the server spells it
/// `stream.set_read_timeout(AUTHED_IDLE_TIMEOUT)`, a replacement that cannot be forgotten.
///
/// Why not a bigger number: the handshake bound exists to stop an UNAUTHENTICATED peer pinning a
/// thread; after `AuthOk` the peer holds a scoped key and is entitled to the thread. A control
/// client parks in its command channel, not a socket read, so it cannot know it must speak, and a
/// finite server timeout would need a client-side pinger. Any finite value only makes the same
/// silent kill rarer and harder to attribute (five minutes killed an agent's connection across one
/// long think).
///
/// # ⚠ What it costs — an ACCEPTED, unbounded node-side leak
///
/// A silently-dead authed connection that is not a SUBSCRIBED observer (a control connection, or
/// an observe peer that never subscribes) pins its node connection thread and its
/// `MAX_CONNECTIONS` reservation FOREVER. `MAX_CONNECTIONS` does not bound the leak, it bounds how
/// many leaks wedge the node, after which every new connection (the operator's too) is refused
/// until a restart. A subscribed observer is not exposed ([`OBSERVE_HEARTBEAT`] makes the node
/// write, so its thread dies on a failed write), nor is an unauthenticated peer.
///
/// Accepted because both cures are worse at this size: `SO_KEEPALIVE` needs `socket2` (buy it the
/// day a second reason appears), and a finite reaper would turn the first `vike-desktop` order click
/// after a quiet night into a `NeverSent` nobody reads (its buttons are fire-and-forget), trading a
/// capacity failure for a silent ORDER failure. Reopens when `vike-desktop`'s control path grows
/// the outcome-aware send `vike-cli mcp` has, or on one observed wedge on a real node. The
/// client-side twin is closed: [`CONTROL_REPLY_TIMEOUT`].
pub const AUTHED_IDLE_TIMEOUT: Option<Duration> = None;

/// How often the node re-asserts liveness on a SUBSCRIBED observe stream that has nothing to say: a
/// `Response::Pong` written when no frame went out for this long. CONSUMED BY THE SERVER in
/// `vike-tradehub` (its push writer), never redefined there.
///
/// 15 s: a BUSY node never heartbeats (the publisher's 15 ms poll and the core's ≥16 ms publish
/// cadence already write far faster), the detection being paid for is a human or agent noticing a
/// stale read (tens of seconds), and an IDLE node pays four ~30-byte frames a minute per subscriber.
/// Deliberately NOT tied to the publisher's `POLL_INTERVAL`: that one is about not missing a
/// publish, this one about not being mistaken for a corpse.
pub const OBSERVE_HEARTBEAT: Duration = Duration::from_secs(15);

/// How long a client's subscribed observe stream may be silent before its receive loop treats the
/// link as dead: a socket read timeout, so the EXISTING error arm of
/// `crates/vike-tradehub-client/src/remote_handle.rs`'s `RemoteCoreHandle` loop flips
/// `is_connected`. Nothing new decides anything; the read simply stops blocking forever.
///
/// THREE heartbeats, not one: a missed beat on a saturated box is evidence of load, not death (the
/// `ssh -o ServerAliveCountMax=3` shape), and it bounds a silent death at 45 s.
///
/// ⚠ ARMED ONLY when the node advertises [`crate::proto::FEATURE_OBSERVE_HEARTBEAT`]: deadlining a
/// node that does not heartbeat would tear down a healthy idle stream every 45 s and reconnect.
pub const OBSERVE_READ_TIMEOUT: Duration = Duration::from_secs(3 * OBSERVE_HEARTBEAT.as_secs());

/// How long a CONTROL worker waits for the node's reply to the command it just wrote before it
/// gives that command up and ends the worker — the write half's [`OBSERVE_READ_TIMEOUT`].
///
/// The defect it closes: the worker is a strictly serial write → read loop whose pre-write probe
/// sees only a REPORTED close, so a silently-dead tunnel accepted the write into a kernel buffer and
/// parked the reply read forever. `is_connected` stayed `true`, every later command queued behind
/// the wedge, and `vike-cli mcp` answered each "sent, outcome unknown" for commands that never left
/// the process.
///
/// 30 s: three times [`HANDSHAKE_REPLY_TIMEOUT`] for strictly more node work (limits, risk check, a
/// lowering into the core: no venue, no disk), set by the WORST honest case (a saturated box),
/// because erring short tears down a healthy link to a busy node.
///
/// On expiry the in-flight command is [`crate::CommandOutcome::Disconnected`] — UNKNOWN, it was
/// written and may have executed — never `NeverSent` (a wrong `Disconnected` costs a
/// `node_snapshot`; a wrong `NeverSent` places an order twice). Everything still QUEUED behind it
/// is `NeverSent`, which lets `vike-cli mcp` reconnect and send those on the same call.
///
/// ⚠ It bounds the wedge, it does not erase it: for up to this long a second command offered to
/// the same handle reads "sent, outcome unknown" while it is still queued. The window is finite and
/// self-heals (worker exits → `ControlRejected::Gone` → never-sent → reconnect).
pub const CONTROL_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a client waits for each HANDSHAKE reply (`Welcome`, then `AuthOk`) before failing the
/// connect. Handshake ONLY: `crate::handshake::node_handshake` clears it before handing the authed
/// stream back, so each caller's own policy governs the session.
///
/// The failure it exists for is a half-open tunnel: `ssh -L` accepts the LOCAL connect long after
/// the far end is gone and then delivers nothing (it once blocked the single-threaded
/// `vike-cli mcp` server forever). Ten seconds is ~1000x the answer time (mint 32 CSPRNG bytes; no
/// venue, disk or fold lock), set by the worst honest case, and still visibly finite to a human.
pub const HANDSHAKE_REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long `crate::remote_handle::directory` waits for the node's `Directory` reply once the
/// handshake is done (whose own bound is cleared on the way out). Without it a node that answered
/// the handshake and never the request parked the desktop's directory fetch for the session.
///
/// Same value as [`CONTROL_REPLY_TIMEOUT`], for the same reason: a settings-database read is more
/// work than minting a nonce, the worst honest case sets it, and expiry ends the fetch like any
/// refusal (the desktop asks again a minute later).
pub const DIRECTORY_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

// Compile-time bounds: a RANGE, so a deliberate tweak stays free while the ways these stop being a
// liveness contract at all do not compile.
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
    DIRECTORY_REPLY_TIMEOUT.as_secs() >= HANDSHAKE_REPLY_TIMEOUT.as_secs(),
    "DIRECTORY_REPLY_TIMEOUT must stay POSITIVE and no tighter than the HANDSHAKE reply it follows \
     — reading a settings database is more node work than minting a nonce"
);
const _: () = assert!(
    CONTROL_REPLY_TIMEOUT.as_secs() >= HANDSHAKE_REPLY_TIMEOUT.as_secs(),
    "CONTROL_REPLY_TIMEOUT must stay POSITIVE and no tighter than the HANDSHAKE reply it follows — \
     zero is a read that can never succeed, and a command reply is strictly MORE node work than \
     minting a nonce, so a value under the handshake's would reap healthy links to a busy node and \
     report every command it reaped as UNKNOWN"
);
