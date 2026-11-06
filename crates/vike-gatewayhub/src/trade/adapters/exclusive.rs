//! **A resource only ONE account of a venue may hold per process** — which account holds it, and
//! the claim. Generic over `VenueDeclaration::process_exclusive`; dukascopy's JForex sidecar is the
//! one such resource today (`crates/bridges/dukascopy/src/adapter.rs`'s module doc carries the
//! evidence and the measurement that would retire the limit). The claim is RAII: a guard is released
//! when it drops, so a failed start never refuses the next account with an untrue reason, and a
//! running resource's claim rides its client ([`hold_while_running`]) and ends with it. It is
//! keyed on the RESOURCE (`ProcessExclusive::resource`), not the venue: two venues declaring one
//! resource exclude each other.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use vike_model::accounts::account_keys::AccountLabel;

use super::registry::VenueRow;
use crate::trade::adapters::snapshot::TradeSnapshot;

/// Which armed LABELLED account holds the resource. `armed`: every labelled account whose arming
/// stands on its own merits (operator-armed, store-identified, credentialed), in label order:
///
/// * **none** → [`AccountLabel::Default`] (every deployment today; it starts `DUKASCOPY_DEMO1_*`).
/// * **exactly one** → that account, and **the DEFAULT account declines**.
/// * **more than one** → the FIRST in label order; the rest decline.
///
/// # ⚠ Why the DEFAULT account declines, which is the whole point
///
/// The fan-out starts the default account FIRST ([`crate::accounts_to_run`],
/// `vike_config::account_arming::known_accounts`), and both accounts' rows are active at a demo tier on any box
/// trading two dukascopy books, so a first-come claim let the default take the sidecar every time:
/// **no configuration could start the second account** — the feature shipped unreachable. An
/// ACTIVE labelled `account` row (dukascopy's second book: `label` = its book number) means *trade
/// this account*, so the unnamed one yields; refusing BOTH would punish a clearly stated
/// configuration.
///
/// ⚠ **The cost**: on a box arming a labelled dukascopy account, the DEFAULT account does NOT RUN
/// (decision 0156) where it used to trade. Loud (an `error!` naming the holder;
/// `vike_config::ArmingBlock::SidecarHeldElsewhere` via `VenueArming::why`), and a strategy mount
/// on it refuses the node (`crates/vike-tradehub-core/src/node/accounts.rs`'s
/// `refuse_unarmed_mount_accounts`). To get the default back, deactivate the labelled row
/// (`vike-cli secrets account deactivate --id N`).
///
/// # More than one is a PICK, and picking is legitimate here
///
/// Elsewhere in dukascopy a guess would pick a LEGAL ENTITY, so it refuses
/// (`crates/bridges/dukascopy/src/account.rs`'s `DukascopyRefusal`); here each account keeps its
/// own key family and only a PROCESS RESOURCE is rationed. The loser does not run, the projection
/// says so, and a strategy naming it is refused (`refuse_unarmed_mount_accounts`, above).
pub(crate) fn pick_holder(armed: &[AccountLabel]) -> AccountLabel {
    armed.first().cloned().unwrap_or(AccountLabel::Default)
}

/// Which account of `venue` holds this process's exclusive resource, from the `account` table.
///
/// Candidates: LABELLED accounts whose own tier (`vike_config::account_arming`'s `account_tier`) passes
/// `crate::arming`'s `account_arming_raw`, which does NOT ask the exclusive question (that is what
/// makes this terminate). A labelled account that cannot arm — an inactive or paper row, no keys —
/// takes nothing from the default. Cost: one key parse + one `account_arming_raw` per labelled
/// account, per row — arithmetic over maps already held.
pub(crate) fn holder(
    registry: &'static [VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&TradeSnapshot>,
) -> AccountLabel {
    // No policy = no account table: the DEFAULT account holds it.
    let Some(p) = policy else { return AccountLabel::Default };
    let armed: Vec<AccountLabel> =
        vike_config::account_arming::known_accounts(venue, Some(&p.accounts))
            .into_iter()
            .filter(|l| !l.is_default())
            .filter(|l| {
                // An account the table does not run, or runs at `paper`, holds no session.
                vike_config::account_arming::account_tier(Some(&p.accounts), venue, l).is_ok_and(
                    |tier| {
                        super::arming::account_arming_raw(registry, venue, l, vars, tier, Some(p))
                            .0
                            .is_some_and(|t| t != vike_model::AccountTier::Paper)
                    },
                )
            })
            .collect();
    pick_holder(&armed)
}

/// Whether `label` holds `venue`'s resource.
pub(crate) fn holds(
    registry: &'static [VenueRow],
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    policy: Option<&TradeSnapshot>,
) -> bool {
    *label == holder(registry, venue, vars, policy)
}

/// The resources this process has claimed, by `ProcessExclusive::resource`.
static CLAIMED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A held claim, RELEASED on drop.
///
/// ⚠ **The release is the point (a live defect when missing):** a never-cleared flag let a FAILED
/// start (absent jar, `Command::spawn` error, `Fatal` login envelope, `READY_TIMEOUT`) refuse every
/// later account with *this process already runs one*. RAII, not `release()` on the error arm: a
/// `return` added between claim and start would silently bring the leak back.
///
/// ⚠ **A running resource's claim is HANDED to its client** ([`hold_while_running`]) and lives exactly as
/// long as that client does. The claim used to be committed for "the life of the process", and the
/// process was the tradehub, which a restart killed together with its JForex child; the process is the
/// gatewayhub now, which outlives every tradehub, so a claim kept past the adapter would refuse the next
/// start (`already claimed`) until the gatewayhub itself restarted.
#[derive(Debug)]
pub(crate) struct ExclusiveClaim<'a> {
    claimed: &'a Mutex<Vec<String>>,
    resource: String,
}

impl Drop for ExclusiveClaim<'_> {
    fn drop(&mut self) {
        // A poisoned set is still a plain list of names — see [`claim_in`].
        let mut claimed = self.claimed.lock().unwrap_or_else(PoisonError::into_inner);
        claimed.retain(|r| r != &self.resource);
    }
}

/// **The claim of a RUNNING resource, held by the client that runs it.** A `Live` outcome's client is
/// wrapped in [`ClaimedClient`], which owns the claim and releases it when the client is dropped (the
/// session's adapter ended: a final detach, the orphan TTL, a restart after a bridge panic, or the
/// daemon stopping); any other outcome drops the claim here, so a start that came up empty frees the
/// resource at once.
pub(crate) fn hold_while_running(
    claim: ExclusiveClaim<'static>,
    outcome: vike_bridge_core::venue_adapter::AdapterOutcome,
) -> vike_bridge_core::venue_adapter::AdapterOutcome {
    use vike_bridge_core::venue_adapter::{AdapterOutcome, ExecOutcome};
    let AdapterOutcome { exec, recon, identity } = outcome;
    let exec = match exec {
        ExecOutcome::Live(mut live) => {
            live.client = Box::new(ClaimedClient { inner: live.client, _claim: claim });
            ExecOutcome::Live(live)
        }
        ExecOutcome::Paper => ExecOutcome::Paper,
    };
    AdapterOutcome { exec, recon, identity }
}

/// A bound exec client that owns its process-exclusive claim. Every door delegates, each explicitly (a
/// trait default here would shadow the inner client's override). ⚠ Field order is the release order:
/// the inner client is dropped (its sidecar torn down) BEFORE the claim is released, so the next start
/// never races a resource that is still going away.
pub(crate) struct ClaimedClient {
    inner: Box<dyn vike_exec::ExecutionClient + Send>,
    _claim: ExclusiveClaim<'static>,
}

impl vike_exec::ExecutionClient for ClaimedClient {
    fn submit(&mut self, request: &vike_model::OrderRequest) {
        self.inner.submit(request);
    }
    fn submit_batch(&mut self, requests: &[vike_model::OrderRequest]) {
        self.inner.submit_batch(requests);
    }
    fn modify(
        &mut self,
        order: &vike_model::OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) {
        self.inner.modify(order, new_qty, new_price);
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.inner.cancel(client_order_id);
    }
    fn cancel_with_intent(&mut self, client_order_id: &str, intent: vike_exec::CancelIntent) {
        self.inner.cancel_with_intent(client_order_id, intent);
    }
    fn cancel_batch(&mut self, client_order_ids: &[String]) {
        self.inner.cancel_batch(client_order_ids);
    }
    fn cancel_batch_with_intent(
        &mut self,
        client_order_ids: &[String],
        intent: vike_exec::CancelIntent,
    ) {
        self.inner.cancel_batch_with_intent(client_order_ids, intent);
    }
    fn confirm(&mut self, client_order_id: &str) {
        self.inner.confirm(client_order_id);
    }
    fn begin_detach(&mut self) {
        self.inner.begin_detach();
    }
    fn detach(&mut self) {
        self.inner.detach();
    }
    fn poll_events(&mut self) -> Option<vike_model::events::Event> {
        self.inner.poll_events()
    }
    fn on_bar(&mut self, bar: &vike_marketdata::Bar) {
        self.inner.on_bar(bar);
    }
    fn amend_semantics(&self) -> Option<vike_venues::AmendSemantics> {
        self.inner.amend_semantics()
    }
    fn wire_id_for(&self, coid: &str) -> Option<String> {
        self.inner.wire_id_for(coid)
    }
    fn adopt_restored(&mut self, orders: &[(String, String)]) {
        self.inner.adopt_restored(orders);
    }
}

/// The process-level backstop: `Some` for the first caller per resource. Never fires on a
/// correctly-resolved start ([`holder`] names one account; the start refuses the rest first); it
/// covers a process starting the venue TWICE, and two venues declaring the same resource.
///
/// ⚠ That second case is excluded HERE ONLY: [`holder`]/[`holds`] work per venue, so the
/// projection shows both holders armed and the second to start does not run
/// (`ArmingBlock::AdapterFailed`). No two declarations share a resource today.
#[must_use]
pub(crate) fn claim(resource: &str) -> Option<ExclusiveClaim<'static>> {
    claim_in(&CLAIMED, resource)
}

/// [`claim`] over a caller-supplied set — the test seam.
///
/// ⚠ A POISONED set is recovered, not read as "held": a plain list of names cannot be left
/// half-written, and `None` would refuse this account with an untrue *already claimed*.
#[must_use]
pub(crate) fn claim_in<'a>(
    claimed: &'a Mutex<Vec<String>>,
    resource: &str,
) -> Option<ExclusiveClaim<'a>> {
    let mut held = claimed.lock().unwrap_or_else(PoisonError::into_inner);
    if held.iter().any(|r| r == resource) {
        return None;
    }
    held.push(resource.to_string());
    Some(ExclusiveClaim { claimed, resource: resource.to_string() })
}

#[path = "exclusive_tests.rs"]
#[cfg(test)]
mod exclusive_tests;

#[path = "exclusive_fold_tests.rs"]
#[cfg(test)]
mod exclusive_fold_tests;
