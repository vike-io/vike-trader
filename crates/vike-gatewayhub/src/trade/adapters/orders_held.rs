//! **THE ORDER HOLD** — what the start does with an arm whose probe answered
//! `vike_bridge_core::venue_adapter::ArmingHeldBelowLive::OrdersHeld`: bind the tier, send no order
//! (`docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`, stage A).
//!
//! `crate::contract`'s `resolution_to_arming` folds that answer to a row AT the bridge's tier with
//! `vike_model::ArmingBlock::OrdersHeld`, and `crate::contract`'s `contract_parts` hands every
//! outcome of such a row to [`hold_orders`], which wraps a `Live` outcome's client in the shared
//! [`ExecutionClientOrdersHeld`] (its module doc says what each door does: every submit and modify
//! refused in [`ORDERS_HELD_REASON`]'s words, every cancel and confirm delegated). The bridge's own
//! client refuses orders too; this one stands IN FRONT of it, so a bridge whose `start` forgot its
//! hold still sends nothing. It is the tier interlock's rule applied to orders: the probe and the
//! arm are one answer, and the fold holds the arm to it.

use vike_bridge_core::execution_client::orders_held::ExecutionClientOrdersHeld;
use vike_bridge_core::venue_adapter::{AdapterOutcome, VenueAdapterExecutionOutcome};
use vike_execution::EventSender;
use vike_model::accounts::account_keys::AccountLabel;

/// The reason every held order is rejected with — the sentence a strategy, the GUI and the
/// journal read, so it says what happened and that it is deliberate.
pub(crate) const ORDERS_HELD_REASON: &str = "orders held: this venue's arm is mounted with every \
     order refused (its order path at this tier is not proven); nothing was sent";

/// A held arm's outcome: a `Live` outcome's client wrapped in [`ExecutionClientOrdersHeld`], said
/// once at `warn!`; a `Paper` outcome unchanged (it has no client to hold).
pub(crate) fn hold_orders(
    venue: &str,
    account: &AccountLabel,
    events: &EventSender,
    outcome: AdapterOutcome,
) -> AdapterOutcome {
    let AdapterOutcome { exec, reconcile: recon, identity } = outcome;
    let exec = match exec {
        VenueAdapterExecutionOutcome::Running(mut live) => {
            tracing::warn!(
                venue,
                account = %account,
                bound_tier = live.bound_tier.as_str(),
                "{venue}: account `{account}` is mounted at the {} tier with every ORDER HELD — \
                 its data and reconcile run, and every order is rejected before it reaches the \
                 venue",
                live.bound_tier.as_str().to_uppercase()
            );
            live.client = Box::new(ExecutionClientOrdersHeld::new(
                live.client,
                events.clone(),
                venue,
                account.to_string(),
                ORDERS_HELD_REASON,
            ));
            VenueAdapterExecutionOutcome::Running(live)
        }
        VenueAdapterExecutionOutcome::NotRunning => VenueAdapterExecutionOutcome::NotRunning,
    };
    AdapterOutcome { exec, reconcile: recon, identity }
}
