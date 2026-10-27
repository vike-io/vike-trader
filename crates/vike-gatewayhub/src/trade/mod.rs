//! The TRADE half of the daemon (feature `trade`, wave 2): the order and execution sessions the tradehub
//! reaches through `vike-gatewayhub-client`'s `trade` feature.
//!
//! This module is written with NO dependency on `crate::server` (decision
//! `docs/decisions/0165-vike-mount-splits-and-no-other-hub-gets-a-core-crate.md`, verdict 1): it takes
//! plain values and closures and hands back plain values, and the wire adapter that owns sockets and
//! threads is `crate::server::trade`. `tests/trade_boundary.rs` reads this directory and refuses the path.
//!
//! # The shape
//!
//! One [`session::TradeHub`] per daemon holds one [`session::AccountSession`] per `(venue, account)`. A
//! session OUTLIVES its connections: it owns the venue adapter (started by the client's `AdapterStart`),
//! the [`ring::EventRing`] the adapter's events land in, the dispatch thread that drives the adapter's
//! `ExecutionClient`, and the reconcile worker. A [`lease::Lease`] is one connection's hold on a session:
//! it reads the client's frames (`Lease::handle`) and is pulled by the connection's writer
//! (`Lease::next`). A link flap therefore costs the client a reconnect and a replay from the ring, not
//! an adapter restart, and a resting order is never cancelled because a link dropped (owner ruling D6).
//!
//! ```text
//!  bridge pumps --EventSender--> forwarder thread --> OnceMemory --> EventRing (seq stamped, bounded)
//!                                                        |  pulled by seq
//!  client --TradeUp--> Lease::handle --> dispatch thread --> ExecutionClient     Lease::next --> writer --> client
//!                           \--> reconcile worker --> ReconClient
//! ```
//!
//! * [`lane`] — [`lane::TradeLane`], the seam to the venue half (arming, preflight, the start of one
//!   adapter), the twin of `crate::data::DataLane` and `crate::rest::RestLane`. The real lane is
//!   [`adapters::bridge_trade::BridgeTrade`] (W9b); `crate::testkit::trade::ScriptedTrade` is the scripted one.
//! * [`adapters`] — the venue half the real lane is made of (the arming table, the preflight probes, the start
//!   of one adapter): the only part of this module that names a bridge.
//! * [`session`] — the hub, the per-account session, the adapter's lifetime, the forwarder.
//! * [`lease`] — one connection's hold: frame handling with every client-named value bounded, the
//!   outbound queue, the fence and the heartbeat clock.
//! * [`ring`], [`dedup`], [`once`] — the event ring, the recent-submit set and the first-wins memory in
//!   front of the ring (one record per fact; pure).
//! * [`dispatch`] — the thread that owns the `ExecutionClient`: dedup, the HALT gate, `Budget::spend`.
//! * [`resolve`] — `WireIds` and `ResolveOrders` (pure helpers).
//! * [`recon`] — the reconcile worker: progress, paging, NOT READY versus failed.
//! * [`state_dir`] — the live-account lock and the project token.
//! * [`ibkr`] — IBKR's gateway address and the [`lane::LinkWatch`] over it (decision 0164 verdict 7(b)): the
//!   daemon reconnects the adapter after the gateway's daily logoff while the account rows say it runs (the
//!   loop is `crate::session::reconnect`, the session half is `session/link.rs`).
//!
//! # Rules that hold in every file
//!
//! * **No order may silently vanish.** A command that cannot reach the adapter (no adapter, a dead
//!   dispatch thread) becomes a terminal `OrderRejected` (a cancel or modify: a non-terminal advisory)
//!   in the ring, in order with the venue's own events.
//! * **Nothing the client names is trusted**: every string and count is bounded before it is used, and
//!   no handler indexes, unwraps or `expect`s on wire data. A violation is `Bye(Protocol(..))`.
//! * **Threads are named** (`gwh-trade-*`) and none is spawned bare.
//! * **Log prefixes are fixed**: `TRADE-OPEN:` for a lease open (accepted or refused) and `TRADE-LINK:`
//!   for everything about a lease's life after it (lost, fenced, taken over, ended). A credential value
//!   appears in neither.

use std::path::PathBuf;
use std::time::Duration;

pub mod adapters;
pub mod dedup;
pub mod dispatch;
pub mod ibkr;
pub mod lane;
pub mod lease;
pub mod once;
pub mod recon;
pub mod resolve;
pub mod ring;
pub mod session;
pub mod state_dir;

/// The default of [`TradeConfig::account_ccy_retry_after`]: a running adapter whose account currency came back
/// empty is asked about it again at its next start, but not more often than this. A miss is a failed moment (a
/// stalled host, a dropped packet), each ask is a whole authenticated venue session, and the next start of a
/// running adapter is the node's link coming back or an engine restarting, so thirty seconds is a pause long
/// enough that a flapping link does not become a flood of sessions and short enough that the first start after
/// a recovered venue heals the anchor. A judgement call, UNMEASURED.
pub const ACCOUNT_CCY_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Every number and path the trade sessions run with. The server derives it from its own
/// `ServerConfig` (the policy numbers live in `server/limits.rs`); this module reads nothing else.
#[derive(Debug, Clone)]
pub struct TradeConfig {
    /// The heartbeat period stated in `TradeOpened.heartbeat_ms`; the daemon writes a `Heartbeat` after
    /// this much silence and the client pings at the same period.
    pub heartbeat: Duration,
    /// The floor of the daemon's read deadline on a lease: it ends a lease silent for
    /// `max(read_floor, 3 x heartbeat)` (a ghost client cannot hold a lease).
    pub read_floor: Duration,
    /// How many events one account's ring holds.
    pub ring_events: usize,
    /// How many recent client order ids one account remembers for `Submit` dedup.
    pub dedup_coids: usize,
    /// Commands queued for one account's dispatch thread. Past it the reader waits (back-pressure on
    /// the client's writer), never drops.
    pub command_queue: usize,
    /// Control frames (replies, acks) queued for one lease. Past it the lease is ended `TooSlow`.
    pub control_queue: usize,
    /// The channel between the bridge's pumps and the forwarder: a bridge's `blocking_send` waits here
    /// exactly as it waits on a slow core in process.
    pub event_channel: usize,
    /// How many leases may be attached at once, daemon-wide.
    pub max_leases: usize,
    /// How long a session with no lease and a running adapter is kept before it is torn down.
    pub orphan_ttl: Duration,
    /// The cadence of `ReconProgress` while a reconcile fetch runs (stated in `next_in_ms`).
    pub recon_progress: Duration,
    /// The byte target of one `ReconReply` page.
    pub recon_page_bytes: usize,
    /// How long `TradeOpen` waits for the previous holder's connection to be read to its end.
    pub fence_drain: Duration,
    /// How long after an `account_currency` ask that came back EMPTY the next start of the SAME running adapter
    /// asks again ([`ACCOUNT_CCY_RETRY_AFTER`] in production): a miss is not kept for the adapter's life, which
    /// would leave the node refusing every symbol quoted in another currency until the daemon restarts. The
    /// retry is single-flight (it runs under the session's adapter lock); an adapter a restore installed, which
    /// asked nothing, is due at once.
    pub account_ccy_retry_after: Duration,
    /// The project's state directory (`LIVE-*.lock`, `gateway-id`); `None` = no project resolved.
    pub state_dir: Option<PathBuf>,
    /// The HALT sentinel's path, resolved once by `vike_boot::halt`; `None` = no gate at this seam.
    pub halt_path: Option<PathBuf>,
}
