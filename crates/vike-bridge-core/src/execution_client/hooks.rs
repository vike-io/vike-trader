//! The hooks a venue hands the shared exec actor (`crate::exec_actor::ExecutionClientActor`), so a bridge returns the
//! actor itself instead of a hand-written `ExecutionClient` wrapper that forwards every method (and silently
//! inherits the trait's no-op for any it forgets).
//!
//! Each type is named after the `vike_execution::ExecutionClient` method it serves, and the actor's builder of the
//! same word installs it (`with_before_submit`, `with_adopt_restored`, `with_wire_id_for`). An actor built
//! without one answers that method exactly as the trait's default does, so a hook is opt-in per venue.
//!
//! ⚠ **Every hook here runs on the CALLER's thread**, which is the core's single-writer fold: it returns at
//! once, reaches no network and waits on nothing (a table write, a channel send, a pure function). Work that
//! must block goes to the venue's own exec thread through the command it already has.
//! `crate::exec_actor::ConfirmFn` is the one hook that blocks, which is why the actor runs it on a worker
//! thread of its own; it moves here with the actor in phase 4.

use std::sync::Arc;

use vike_execution::RestoredOrder;
use vike_model::OrderRequest;

/// Serves `ExecutionClient::submit`: runs on the order before the actor reads it, ahead of the HALT check
/// and of the queue, and the actor then uses the order as the hook left it (its HALT reject and the queued
/// command alike). A venue records what its later events need (alpaca and oanda: the symbol a fill must
/// carry, recorded before any fill of the order can be built) or stamps a field of its own (polymarket: `ts`
/// becomes this moment). It never refuses: a refusal is a terminal event, and only the loop sends one.
pub type BeforeSubmitFn = Arc<dyn Fn(&mut OrderRequest) + Send + Sync>;

/// Serves `ExecutionClient::adopt_restored`: teaches the venue's own tables the orders a previous session
/// left resting (an order client id's symbol for the router, a venue order id for the cancel route). Idempotent: a later
/// pass may announce the same order again.
pub type AdoptRestoredFn = Arc<dyn Fn(&[RestoredOrder]) + Send + Sync>;

/// Serves `ExecutionClient::wire_id_for`: the id this venue's reports carry for the order submitted under a
/// order client id, when that id is a pure function of the order client id and not the order client id itself (hyperliquid's `keccak`).
/// Pure and cheap: called at reconcile-pass cadence, it allocates no registry state.
pub type WireIdForFn = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
