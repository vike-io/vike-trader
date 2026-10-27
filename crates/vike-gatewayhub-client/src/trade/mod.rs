//! The TRADE half of the client (feature `trade`): what the tradehub plugs in where it used to hold a
//! venue bridge.
//!
//! * [`TradeLink`] / [`TradeSession`] (`link.rs`): one long-lived lease per `(client, account)` under
//!   `Scope::Trade` - dial, open, heartbeats both ways, redial with a client-local retry table, the
//!   event relay, the request/answer plumbing.
//! * [`RemoteExecClient`] (`exec.rs`): the `vike_exec::ExecutionClient` the core drives. Non-blocking,
//!   HALT-checked, NEVER-SENT-aware.
//! * [`RemoteReconClient`] (`recon.rs`): the `vike_exec::recon::ReconClient` the `ReconManager` drives.
//!   Never an empty answer for a fetch that did not happen; NOT READY is distinct from an error.
//! * `ledger.rs`: the NEVER-SENT queue (pure). `cursor.rs`: the ring position and the duplicate guard
//!   (pure). `ids.rs`: the wire-id cache. `heartbeat.rs`: the deadline arithmetic. `shared.rs`: the state
//!   the threads share. `mirror.rs`: the total conversions between wire mirrors and their originals.
//!   `disclosure.rs`: what the core is told when the link goes down and comes back. `rpc.rs`: the arming
//!   and preflight one-shots.
//!
//! # The contract in five lines
//!
//! 1. **No order may silently vanish.** A submit that cannot reach the gatewayhub gets a terminal
//!    `OrderRejected` (NEVER-SENT); one the gatewayhub's watermark proves unread gets one too; one that
//!    was Written and cannot be accounted for gets NOTHING synthesized as a verdict (UNKNOWN; the venue's own word may be preceded by the order's lost prefix, W2F-D1) and resolves through
//!    the venue's own event, `ResolveOrders`, reconcile and the stuck-order watchdog, in that order.
//! 2. **The client never re-sends a Written frame.** The only frames sent twice are never-Written ones
//!    (a held risk-off cancel) and the idempotent `AdoptRestored` teaching.
//! 3. **The fold thread does a channel send and a stat.** Nothing it can reach serializes, writes or locks.
//! 4. **Link loss is disclosed, not acted on.** `FeedStatus::Disconnected` per mounted key and `Live` on
//!    recovery; resting orders are NOT cancelled (owner ruling D6) - reconcile picks them up.
//! 5. **Credentials are parameters.** The Trade key arrives in the [`crate::client::GatewayLink`]; no
//!    store, no environment, no setting is read here, and no key reaches a `Debug` or a log line.

mod cursor;
mod disclosure;
mod exec;
mod heartbeat;
mod ids;
mod ledger;
mod link;
mod mirror;
mod recon;
pub mod rpc;
mod shared;
mod told;

pub use disclosure::{CoreLinkSignal, LinkSignal, MountedKeys, NoSignal};
pub use exec::RemoteExecClient;
pub use ledger::{
    LEASE_CLOSED_CANCEL_REASON, LEASE_CLOSED_MODIFY_REASON, LEASE_CLOSED_SUBMIT_REASON,
    NEVER_RECEIVED_CANCEL_REASON, NEVER_RECEIVED_MODIFY_REASON, NEVER_RECEIVED_SUBMIT_REASON,
    NEVER_SENT_CANCEL_REASON, NEVER_SENT_MODIFY_REASON, NEVER_SENT_SUBMIT_REASON,
    NON_FINITE_MODIFY_REASON, NON_FINITE_SUBMIT_REASON, TOO_LARGE_SUBMIT_REASON,
};
pub use link::{STOP_BOUND, TradeLink, TradeSession};
pub use mirror::resolved_coid;
pub use recon::{
    RECON_LISTS_TRIGGER_ORDERS, RECON_NOT_READY_PREFIX, RECON_READS_ADMITTED, RemoteReconClient,
    is_recon_not_ready,
};
pub use shared::{
    LinkState, SymbolFactsError, TradeAccount, TradeError, TradeLinkStats, TradeTimings,
};
