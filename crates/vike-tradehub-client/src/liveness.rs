//! `liveness` — the ONE home for the node protocol's LINK-LIVENESS facts: how long a connection may
//! be quiet before somebody concludes the link is dead, and how a quiet link proves it is not.
//!
//! # Two kinds of number live here
//!
//! **Protocol facts stay facts, and are not settings keys.** [`OBSERVE_HEARTBEAT`] (the node's own
//! push cadence), the handshake bounds ([`CLIENT_HANDSHAKE_DEADLINE`], `vike_node_proto::limits`'s
//! `CLIENT_CONNECT_TIMEOUT`) and the node's pre-auth caps are numbers both ends must agree on or
//! that guard the node against a peer that has proved nothing: a node that heartbeats every 15 s
//! beside a client that gives up after 10 would tear a healthy link down every quarter minute. They
//! live in this crate, the LOWER of the two, and `vike-tradehub`'s server CONSUMES them, never a
//! second copy (root `CLAUDE.md`'s "Workspace layers (dependency direction: down only)").
//!
//! **The operator's PATIENCE on a bad link is a client preference** (the owner's ruling of
//! 2026-10-13, overruling this page's earlier "not operator knobs"). How long a CLIENT waits in
//! silence on the observe stream ([`OBSERVE_READ_TIMEOUT`]) and for a control reply
//! ([`CONTROL_REPLY_TIMEOUT`]) is a statement about the client's own link — a flaky Wi-Fi or VPN
//! tears the stream down repeatedly at today's value — and a client waiting LONGER than the node's
//! cadence needs is harmless to the node, so no agreement between the ends is lost. The two
//! constants are now the built-in defaults AND the floors of the settings rows
//! `preferences.observe_stall_timeout_ms` and `preferences.control_reply_timeout_ms`: RAISE-ONLY,
//! because a window below them is the failure above (a healthy but loaded link torn down), refused
//! by `vike-config` at the write and at the load, never clamped here. [`LinkPatience`] carries the
//! two resolved windows from a composition root (a binary that read the rows) to the dial, as a
//! PARAMETER; this crate reads no setting.
//!
//! Every relation that mattered holds for any accepted value because the constants are the FLOORS:
//! the compile-time assertions below, and `vike-tradehub`'s `SETTINGS_LOCK_BUDGET` margin (a
//! fraction of [`CONTROL_REPLY_TIMEOUT`]), are argued against them, and a larger window only widens
//! each margin. The one-shot replies ([`DIRECTORY_REPLY_TIMEOUT`], [`ONE_SHOT_REPLY_TIMEOUT`]) are
//! not part of the patience rows and keep their fixed values.
//!
//! The four questions, and who consumes each answer:
//!
//! 1. What does an AUTHENTICATED idle connection experience? [`AUTHED_IDLE_TIMEOUT`] (the node's
//!    connection loop): nothing.
//! 2. How does a SILENT observe stream prove it is alive? [`OBSERVE_HEARTBEAT`] (the node's push
//!    writer) and [`OBSERVE_READ_TIMEOUT`] (`RemoteCoreHandle`'s receive loop).
//! 3. How long may a CONTROL command's reply take? [`CONTROL_REPLY_TIMEOUT`] (the
//!    `remote_control` worker).
//! 4. How long may a HANDSHAKE take? [`CLIENT_HANDSHAKE_DEADLINE`] (`crate::handshake`, shared with
//!    every other node client through `vike_node_proto::limits`); the
//!    directory read's reply, [`DIRECTORY_REPLY_TIMEOUT`] (`remote_handle::directory`); and every
//!    other one-shot verb's reply, [`ONE_SHOT_REPLY_TIMEOUT`].
//!
//! ⚠ **There is no TCP-keepalive backstop in this workspace.** Keepalive is off unless a socket sets
//! `SO_KEEPALIVE`, `std::net::TcpStream` cannot (it is `socket2`'s surface, and this workspace
//! refuses a dependency for one option), and nothing here does (`grep -rn keepalive` over both
//! tradehub crates finds prose and no call). A read parked on a silently-dead
//! socket (a sleeping laptop, a Wi-Fi change, a VPN re-key under an `ssh -L` that keeps its
//! forwarded socket open) with no deadline is parked for the life of the process.

use std::time::Duration;

use vike_node_proto::limits::CLIENT_HANDSHAKE_DEADLINE;

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
/// This is the DEFAULT and the FLOOR of the client's patience: `preferences.observe_stall_timeout_ms`
/// can raise it (a bad link), never lower it, and a client dials with [`LinkPatience::observe_stall`]
/// (`RemoteCoreHandle::connect_with_read_timeout`'s parameter). The compile-time assertion below
/// holds the floor at two heartbeats or more.
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
/// 30 s: three times [`CLIENT_HANDSHAKE_DEADLINE`] for strictly more node work (limits, risk check, a
/// lowering into the core: no venue, no disk), set by the WORST honest case (a saturated box),
/// because erring short tears down a healthy link to a busy node.
///
/// This is the DEFAULT and the FLOOR of the client's patience: `preferences.control_reply_timeout_ms`
/// can raise it, never lower it, and a client dials with [`LinkPatience::control_reply`]
/// (`RemoteControlHandle::connect_with_reply_timeout`'s parameter). The node's own margins are
/// argued against THIS value: `vike-tradehub`'s settings-write budget is held inside a fifth of it,
/// so a raised window only widens that margin.
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

/// How long `crate::remote_handle::directory` waits for the node's `Directory` reply once the
/// handshake is done (whose own bound is cleared on the way out). Without it a node that answered
/// the handshake and never the request parked the desktop's directory fetch for the session.
///
/// Same value as [`CONTROL_REPLY_TIMEOUT`]'s built-in default, for the same reason: a
/// settings-database read is more work than minting a nonce, the worst honest case sets it, and
/// expiry ends the fetch like any refusal (the desktop asks again a minute later). It does NOT
/// follow a raised `preferences.control_reply_timeout_ms`: that row is the patience of the control
/// channel alone, and no relation here depends on the two being equal.
pub const DIRECTORY_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long every OTHER one-shot verb waits for its one reply once the handshake is done
/// (`crate::handshake::one_shot_handshake`): the snapshot, the strategy reads, the settings read,
/// the tearsheet, a preview, a settings write. Without it a node that answered the handshake and
/// then went silent (the half-open `ssh -L` shape, after the `Welcome`) parked the caller for good:
/// `vike-cli mcp` is single-threaded, so one parked read was the whole server.
///
/// 60 s, twice [`DIRECTORY_REPLY_TIMEOUT`], because the heaviest of these verbs is heavier than a
/// settings-database read: the tearsheet renders over the node's whole live journal (unmeasured on
/// a long-running node). Expiry is an error like any refusal (`TimedOut` on windows,
/// `WouldBlock` on unix), and every caller already treats a failed one-shot as "ask again".
pub const ONE_SHOT_REPLY_TIMEOUT: Duration = Duration::from_secs(60);

/// The two windows of the operator's patience on a bad link, resolved: the silence limit of a
/// subscribed observe stream and the wait for a control reply. A composition root (a binary that
/// read `preferences.observe_stall_timeout_ms` and `preferences.control_reply_timeout_ms`) builds
/// one and hands it down; this crate reads no setting.
///
/// [`Default`] is today's behaviour byte for byte ([`OBSERVE_READ_TIMEOUT`] and
/// [`CONTROL_REPLY_TIMEOUT`]), which is what a box with neither row resolves. The constructor does
/// NOT validate: the floor and the ceiling are `vike-config`'s, refused at the write and the load,
/// and a clamp here would be the silent coercion the settings layer refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkPatience {
    /// How long a subscribed observe stream may be silent before its receive loop ends the link;
    /// the `read_timeout` of `RemoteCoreHandle::connect_with_read_timeout`.
    pub observe_stall: Duration,
    /// How long a control worker waits for the reply to the command it wrote; the `reply_timeout`
    /// of `RemoteControlHandle::connect_with_reply_timeout`.
    pub control_reply: Duration,
}

impl LinkPatience {
    /// The patience two settings rows (milliseconds each) resolved to.
    #[must_use]
    pub const fn from_millis(observe_stall_ms: u32, control_reply_ms: u32) -> Self {
        LinkPatience {
            observe_stall: Duration::from_millis(observe_stall_ms as u64),
            control_reply: Duration::from_millis(control_reply_ms as u64),
        }
    }
}

impl Default for LinkPatience {
    fn default() -> Self {
        LinkPatience { observe_stall: OBSERVE_READ_TIMEOUT, control_reply: CONTROL_REPLY_TIMEOUT }
    }
}

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
    DIRECTORY_REPLY_TIMEOUT.as_secs() >= CLIENT_HANDSHAKE_DEADLINE.as_secs(),
    "DIRECTORY_REPLY_TIMEOUT must stay POSITIVE and no tighter than the HANDSHAKE reply it follows \
     — reading a settings database is more node work than minting a nonce"
);
const _: () = assert!(
    ONE_SHOT_REPLY_TIMEOUT.as_secs() >= DIRECTORY_REPLY_TIMEOUT.as_secs()
        && ONE_SHOT_REPLY_TIMEOUT.as_secs() <= 600,
    "ONE_SHOT_REPLY_TIMEOUT must stay no tighter than the directory read (its lightest sibling) \
     and finite enough that a half-open link is reported while somebody is still waiting"
);
const _: () = assert!(
    CONTROL_REPLY_TIMEOUT.as_secs() >= CLIENT_HANDSHAKE_DEADLINE.as_secs(),
    "CONTROL_REPLY_TIMEOUT must stay POSITIVE and no tighter than the HANDSHAKE reply it follows — \
     zero is a read that can never succeed, and a command reply is strictly MORE node work than \
     minting a nonce, so a value under the handshake's would reap healthy links to a busy node and \
     report every command it reaped as UNKNOWN"
);

#[path = "liveness_tests.rs"]
#[cfg(test)]
mod liveness_tests;
