//! The ONE shared venue execution client and its parts, under their final names (the bridge-core tree's
//! `execution_client/` folder).
//!
//! The actor itself is still `crate::exec_actor::ExecutionClientActor`, in `exec_actor.rs`: moving it here and naming it
//! `ExecutionClientActor` touches every bridge, so both wait for phase 4. What is born here takes its final
//! name and path at once:
//!
//! * [`hooks`]: what a venue hands the actor so it needs no `ExecutionClient` wrapper of its own;
//! * [`orders_held`]: the client that sends no order, in front of a bound one;
//! * [`order_client_id`]: the tables a bridge keeps about its orders' ids (`OrderVenueIdRegistry`, `FifoMap`);
//! * [`resync`]: the history replay's walk over an account's symbols.

pub mod hooks;
pub mod order_client_id;
pub mod orders_held;
pub mod resync;
