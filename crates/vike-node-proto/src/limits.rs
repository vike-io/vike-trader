//! The connection limits every node daemon and every node client share — ONE definition each.
//!
//! The pre-auth frame cap, the handshake deadline and the idle read timeout were hand-copied into
//! `vike-tradehub`, `vike-datahub`, `vike-backtesthub` and `vike-strategy-builder` ("mirrors … byte for
//! byte"), and the client dial bounds into `vike-datahub-client` and `vike-strategy-builder`'s
//! client, with no test comparing the copies. They live beside [`crate::frame::MAX_FRAME_LEN`] and
//! the handshake in [`crate::auth`] because they are the same kind of fact: what a node-protocol
//! peer may spend before it has proved anything.
//!
//! Names: `SERVER_*` is what a daemon spends on a peer that has proved nothing; `CLIENT_*` is what a
//! caller spends on a daemon that may be dead. `*_DEADLINE` bounds a handshake phase, `*_TIMEOUT` one
//! socket wait.
//!
//! Two groups, kept apart on purpose. The SERVER side ([`SERVER_HANDSHAKE_MAX_FRAME_LEN`],
//! [`SERVER_HANDSHAKE_DEADLINE`], [`SERVER_IDLE_READ_TIMEOUT`]) bounds what an unauthenticated peer can cost a
//! daemon. The CLIENT side ([`CLIENT_CONNECT_TIMEOUT`], [`CLIENT_HANDSHAKE_DEADLINE`]) bounds how
//! long a caller waits on a dead daemon. A client's deadline equals its server's by choice, not
//! coupling: either may be tuned alone, which is why the client pair has its own names rather than
//! reusing [`SERVER_HANDSHAKE_DEADLINE`].
//!
//! Values are pinned by `limits_tests.rs`; the compile-time RANGES below are the tradehub
//! `confirm.rs` idiom — a deliberate tweak stays free while "the bound was effectively removed" and
//! "the bound refuses every legitimate handshake" both fail to compile.

use std::time::Duration;

/// A generous per-connection read timeout so a half-open or idle peer cannot park a connection
/// thread in `read` for the process lifetime.
///
/// Sized for the localhost + SSH-tunnel deployment: a real request body is kilobytes and arrives in
/// milliseconds once its length prefix is seen, so a timeout realistically only fires BETWEEN
/// requests (an idle or half-open connection), which the loop then closes to free the thread. It is
/// deliberately far larger than any legitimate inter-request gap — a value that would clip a slow
/// but live client is worse than a leaked thread — and a timeout is never recovered into a resumed
/// loop (the stream would be desynced).
///
/// ⚠ It bounds the gap between FRAMES, never the length of a RUN. A sweep can hold this thread for
/// minutes inside one request and no timeout here can fire while it does — the read that would
/// time out has not been issued yet. That is deliberate: a backtesthub whose long answers were
/// clipped by an idle timeout would be useless, and it is why this value is about the CLIENT going
/// quiet rather than about the work taking a while.
///
/// Read by `vike-datahub` and `vike-backtesthub`. `vike-tradehub`'s handshake-phase read timeout is a
/// different fact (it bounds only the unauthenticated phase and is replaced once a scope is
/// granted) and keeps its own constant.
///
/// This is the DEFAULT, not the value a daemon necessarily runs: both daemons take their idle read
/// timeout as a PARAMETER, and a box that wrote the `config.node_idle_read_timeout_ms` row hands them
/// that value through [`server_idle_read_timeout`] instead. A box without the row runs exactly this.
/// The row is accepted only inside [`SERVER_IDLE_READ_TIMEOUT_MIN`]..=[`SERVER_IDLE_READ_TIMEOUT_MAX`].
///
/// ⚠ It is NOT tied to any client's reply bound. `vike-datahub-client`'s `REPLY_IDLE_TIMEOUT` is how
/// long a CLIENT waits for a reply, this is how long a SERVER waits for the next request: different
/// directions, and neither can clip the other. A value here shorter than a client's gap between
/// requests reaps that client's connection instead (see [`SERVER_IDLE_READ_TIMEOUT_MIN`]).
pub const SERVER_IDLE_READ_TIMEOUT: Duration = Duration::from_secs(300);

/// The shortest idle read timeout a `config.node_idle_read_timeout_ms` row may name: two minutes.
///
/// Not a taste: it is TWICE the slowest cadence at which a legitimate client holds one connection
/// across requests. The desktop's News and Calendar tools keep a datahub connection between polls
/// that are a minute apart (`crates/vike-app-core/src/tools/tool_feeds.rs`'s `POLL_EVERY`, held to
/// this floor by a compile-time assert beside it), and a connection reaped between two of its polls
/// costs the next poll an error. A floor AT that cadence would race every poll against the reap, so
/// the floor is twice it. It is also far above [`SERVER_HANDSHAKE_DEADLINE`], whose whole premise is
/// being much shorter than this.
///
/// ⚠ Nothing makes ANY value safe for every client: `RemoteHistStore` (the desktop's Studio, the
/// backtesthub's own reads) reuses one connection for as long as its holder lives and has no
/// keepalive, so a connection idle past the timeout is reaped and the holder's NEXT read fails once
/// (it drops the dead connection and redials on the read after). That is true of the default as much as of
/// a short row, and it is why a shorter value is a trade the operator makes, not a free tuning.
pub const SERVER_IDLE_READ_TIMEOUT_MIN: Duration = Duration::from_secs(120);

/// The longest idle read timeout a `config.node_idle_read_timeout_ms` row may name: one hour.
///
/// A longer one makes a dead peer (a laptop that slept, a tunnel that dropped without a FIN) hold a
/// connection thread, and a `vike-datahub` slot of its small `MAX_CONNECTIONS`, for longer than any
/// legitimate gap between requests needs; it matches the ceiling the clients put on their own idle
/// bound (`vike-datahub-client`'s `REPLY_IDLE_TIMEOUT` assert).
pub const SERVER_IDLE_READ_TIMEOUT_MAX: Duration = Duration::from_secs(3600);

/// The idle read timeout a daemon runs, from its `config.node_idle_read_timeout_ms` row: the row's
/// milliseconds, or [`SERVER_IDLE_READ_TIMEOUT`] when the box wrote none.
///
/// A pure function of its argument, which is the point: the daemons' composition roots read the
/// settings and hand the row here, so nothing in a library reads configuration. The range is NOT
/// checked here — `vike-config` refuses a row outside
/// [`SERVER_IDLE_READ_TIMEOUT_MIN`]..=[`SERVER_IDLE_READ_TIMEOUT_MAX`] at the write and at the load
/// (a pin test in each daemon holds its restated bounds equal to these), so a value that reaches
/// this function has already passed, and a test may hand a daemon a shorter one directly.
#[must_use]
pub fn server_idle_read_timeout(row_ms: Option<u32>) -> Duration {
    row_ms.map_or(SERVER_IDLE_READ_TIMEOUT, |ms| Duration::from_millis(u64::from(ms)))
}

/// Frame ceiling for the PRE-AUTH phase — the `Hello` and the `Auth` an unauthenticated peer sends
/// to a KEYED server.
///
/// The shared [`crate::frame::MAX_FRAME_LEN`] is 64 MiB because a legitimate datahub *answer* (a
/// chart's worth of bars) can be large. A handshake frame cannot be: `Hello` is a version number
/// and `Auth` is a scope plus a 32-byte mac, together a few hundred bytes even JSON-encoded.
/// Accepting 64 MiB of them means anyone who can reach the socket makes the server allocate 64 MiB
/// per connection by sending FOUR BYTES — no key, no round trip. 64 KiB is ~200x the largest real
/// handshake frame and 1024x cheaper to be wrong about.
///
/// Post-auth frames keep the full [`crate::frame::MAX_FRAME_LEN`]: by then the peer has proved
/// possession of a scoped key, and a profile TOML, a Rhai source or an operator `Command`
/// rationale legitimately runs to kilobytes.
///
/// ⚠ It applies ONLY on a KEYED server. A key-less one has no pre-auth phase at all — every frame
/// is a post-auth frame by definition — so imposing it there would shrink a limit that legitimate
/// traffic already uses, for no security gain behind the loopback guard.
///
/// Read by the tradehub node, the datahub, the backtesthub and the strategy-builder daemon.
pub const SERVER_HANDSHAKE_MAX_FRAME_LEN: u32 = 64 * 1024;

/// How long a KEYED server waits for the whole two-frame handshake before closing the connection.
///
/// Deliberately FAR shorter than [`SERVER_IDLE_READ_TIMEOUT`] (300 s by default, sized for the gap BETWEEN
/// requests on a live client; never below [`SERVER_IDLE_READ_TIMEOUT_MIN`], which the assert below holds
/// above this): an unauthenticated peer has nothing to think about — the `Hello` is a
/// constant and the `Auth` is one HMAC over 32 bytes it was just handed — so a handshake that has
/// not completed in this window is not slow, it is a socket somebody opened and said nothing on.
/// Without it, the idle read timeout would let an unauthenticated peer park a connection thread for
/// minutes per socket.
///
/// Applied as the stream's read timeout for the pre-auth phase only, then RESET to the daemon's idle
/// read timeout (default [`SERVER_IDLE_READ_TIMEOUT`]) once `AuthOk` is written — so it bounds the
/// handshake without clipping a legitimately idle authenticated client.
///
/// Read by `vike-datahub` and `vike-backtesthub`; the datahub's every production entry passes this
/// constant, and its `serve_authed_with_handshake_deadline` is the one that takes another so a test
/// can watch the deadline fire in half a second rather than ten.
pub const SERVER_HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// How long ONE resolved socket address gets for its TCP handshake before a client tries the next
/// candidate.
///
/// It replaces the OS SYN-retry default (MINUTES on Linux) that a black-holed host stalls on. A live
/// peer — loopback, a LAN, an SSH tunnel — answers in milliseconds and the failure it bounds is
/// binary, so it is not tuned finer. PER RESOLVED ADDRESS because a name can resolve to both an A
/// and an AAAA record, and a client that tried only the first would fail against a dual-stack host
/// whose IPv6 route is dark.
///
/// Read by [`crate::frame::connect_bounded`], the one dial of `vike-datahub-client`,
/// `vike-strategy-builder`'s client and `vike-tradehub-client`.
pub const CLIENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a CLIENT gives EACH read and EACH write of the protocol handshake — `Hello`/`Welcome`
/// plus, on a keyed server, `Auth`/`AuthOk`. Armed as BOTH socket timeouts for that phase only,
/// then replaced by the client's own request timeouts.
///
/// ⚠ A socket timeout is PER SYSCALL: a keyed handshake is six bounded calls (each sent frame one
/// `write_all`, each received one two `read_exact`s), so a peer answering each just inside the
/// window stretches the exchange to about six times this value. Accepted: that peer is answering.
///
/// The client mirror of [`SERVER_HANDSHAKE_DEADLINE`] (nothing is COMPUTED during a handshake), equal by
/// choice, not coupling: either may be tuned alone.
///
/// Read by [`crate::frame::connect_bounded`] and, as the reply bound of the handshake exchange, by
/// `vike-datahub-client` and `vike-tradehub-client`.
pub const CLIENT_HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

const _: () = assert!(
    SERVER_HANDSHAKE_MAX_FRAME_LEN > 0 && SERVER_HANDSHAKE_MAX_FRAME_LEN <= 1024 * 1024,
    "SERVER_HANDSHAKE_MAX_FRAME_LEN must stay far under MAX_FRAME_LEN — an unauthenticated peer must not \
     be able to name a large allocation"
);
const _: () = assert!(
    SERVER_HANDSHAKE_DEADLINE.as_secs() > 0 && SERVER_HANDSHAKE_DEADLINE.as_secs() <= 120,
    "SERVER_HANDSHAKE_DEADLINE must stay a POSITIVE, short bound — it exists to stop an unauthenticated \
     peer parking a connection thread, which a 0 (refuse everything) or a multi-minute value both \
     defeat"
);
const _: () = assert!(
    SERVER_IDLE_READ_TIMEOUT_MIN.as_millis() <= SERVER_IDLE_READ_TIMEOUT.as_millis()
        && SERVER_IDLE_READ_TIMEOUT.as_millis() <= SERVER_IDLE_READ_TIMEOUT_MAX.as_millis(),
    "SERVER_IDLE_READ_TIMEOUT must lie inside the range a `config.node_idle_read_timeout_ms` row is \
     accepted in, or a box without the row would run a value no box may write"
);
const _: () = assert!(
    SERVER_IDLE_READ_TIMEOUT_MIN.as_millis() > 4 * SERVER_HANDSHAKE_DEADLINE.as_millis(),
    "the idle read timeout's floor must stay well above SERVER_HANDSHAKE_DEADLINE: the handshake bound \
     exists to be far shorter than the idle one, and the RESET to the idle value after `AuthOk` must \
     never LOWER the bound on an authenticated peer below the one it just passed"
);
const _: () = assert!(
    CLIENT_CONNECT_TIMEOUT.as_secs() > 0 && CLIENT_CONNECT_TIMEOUT.as_secs() <= 60,
    "CLIENT_CONNECT_TIMEOUT must stay a POSITIVE, short bound — it exists to replace the OS \
     SYN-retry default, which a 0 (refuse everything) and a multi-minute value both fail to improve \
     on"
);
const _: () = assert!(
    CLIENT_HANDSHAKE_DEADLINE.as_secs() > 0 && CLIENT_HANDSHAKE_DEADLINE.as_secs() <= 120,
    "CLIENT_HANDSHAKE_DEADLINE must stay a POSITIVE, short bound — nothing is COMPUTED during the \
     handshake, so a long value only lengthens how long a silent peer parks the calling thread"
);

#[cfg(test)]
#[path = "limits_tests.rs"]
mod tests;
