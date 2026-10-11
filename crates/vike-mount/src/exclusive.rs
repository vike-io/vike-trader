//! **A resource only ONE account of a venue may hold per process** — which account holds it, and
//! the claim. Generic over `VenueDeclaration::process_exclusive`; dukascopy's JForex sidecar is the
//! one such resource today (`crates/bridges/dukascopy/src/mount.rs`'s module doc carries the
//! evidence and the measurement that would retire the limit). The claim is RAII: a guard not kept
//! is released, so a failed start never refuses the next account with an untrue reason. It is
//! keyed on the RESOURCE (`ProcessExclusive::resource`), not the venue: two venues declaring one
//! resource exclude each other.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use vike_model::accounts::account_keys::AccountLabel;

use crate::{MountPolicy, VenueRow};

/// Which armed LABELLED account holds the resource. `armed`: every labelled account whose arming
/// stands on its own merits (operator-armed, store-identified, credentialed), in label order:
///
/// * **none** → [`AccountLabel::Default`] (every deployment today; it mounts `DUKASCOPY_DEMO1_*`).
/// * **exactly one** → that account, and **the DEFAULT account declines**.
/// * **more than one** → the FIRST in label order; the rest decline.
///
/// # ⚠ Why the DEFAULT account declines, which is the whole point
///
/// The fan-out mounts the default account FIRST ([`crate::accounts_to_mount`],
/// [`crate::arming::known_accounts`]), and both accounts' rows are active at a demo tier on any box
/// trading two dukascopy books, so a first-come claim let the default take the sidecar every time:
/// **no configuration could mount the second account** — the feature shipped unreachable. An
/// ACTIVE labelled `account` row (dukascopy's second book: `label` = its book number) means *trade
/// this account*, so the unnamed one yields; refusing BOTH would punish a clearly stated
/// configuration.
///
/// ⚠ **The cost**: on a box arming a labelled dukascopy account, a strategy on the DEFAULT account
/// runs on PAPER where it used to trade. Loud (an `error!` naming the holder;
/// `vike_config::ArmingBlock::SidecarHeldElsewhere` via `VenueArming::why`), but
/// `crates/vike-mount/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts` does NOT refuse it
/// (it skips mounts naming no account). To get the default back, deactivate the labelled row
/// (`vike-cli secrets account deactivate --id N`).
///
/// # More than one is a PICK, and picking is legitimate here
///
/// Elsewhere in dukascopy a guess would pick a LEGAL ENTITY, so it refuses
/// (`crates/bridges/dukascopy/src/account.rs`'s `DukascopyRefusal`); here each account keeps its
/// own key family and only a PROCESS RESOURCE is rationed. The loser resolves PAPER, the projection
/// says so, and a strategy naming it is refused (`refuse_unarmed_mount_accounts`, above).
pub(crate) fn pick_holder(armed: &[AccountLabel]) -> AccountLabel {
    armed.first().cloned().unwrap_or(AccountLabel::Default)
}

/// Which account of `venue` holds this process's exclusive resource, from the `account` table.
///
/// Candidates: LABELLED accounts whose own tier (`crate::arming`'s `account_tier`) passes
/// `crate::arming`'s `account_arming_raw`, which does NOT ask the exclusive question (that is what
/// makes this terminate). A labelled account that cannot arm — an inactive or paper row, no keys —
/// takes nothing from the default. Cost: one key parse + one `account_arming_raw` per labelled
/// account, per row — arithmetic over maps already held.
pub(crate) fn holder(
    registry: &'static [VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> AccountLabel {
    // No policy = no account table: the DEFAULT account holds it.
    let Some(p) = policy else { return AccountLabel::Default };
    let armed: Vec<AccountLabel> = crate::arming::known_accounts(venue, vars, Some(p))
        .into_iter()
        .filter(|l| !l.is_default())
        .filter(|l| {
            let tier = crate::arming::account_tier(Some(p), venue, l).0;
            crate::arming::account_arming_raw(registry, venue, l, vars, tier, Some(p)).0
                != vike_config::VenueMode::Paper
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
    policy: Option<&MountPolicy>,
) -> bool {
    *label == holder(registry, venue, vars, policy)
}

/// The resources this process has claimed, by `ProcessExclusive::resource`.
static CLAIMED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A held claim, RELEASED on drop unless [`ExclusiveClaim::keep`] commits it.
///
/// ⚠ **The release is the point (a live defect when missing):** a never-cleared flag let a FAILED
/// start (absent jar, `Command::spawn` error, `Fatal` login envelope, `READY_TIMEOUT`) refuse every
/// later account with *this process already runs one*. RAII, not `release()` on the error arm: a
/// `return` added between claim and start would silently bring the leak back.
#[derive(Debug)]
pub(crate) struct ExclusiveClaim<'a> {
    claimed: &'a Mutex<Vec<String>>,
    resource: String,
    keep: bool,
}

impl ExclusiveClaim<'_> {
    /// Commit the claim for the life of the process — called once the resource is running.
    pub(crate) fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for ExclusiveClaim<'_> {
    fn drop(&mut self) {
        if !self.keep {
            // A poisoned set is still a plain list of names — see [`claim_in`].
            let mut claimed = self.claimed.lock().unwrap_or_else(PoisonError::into_inner);
            claimed.retain(|r| r != &self.resource);
        }
    }
}

/// The process-level backstop: `Some` for the first caller per resource. Never fires on a
/// correctly-resolved mount ([`holder`] names one account; the mount refuses the rest first); it
/// covers a process mounting the venue TWICE, and two venues declaring the same resource.
///
/// ⚠ That second case is excluded HERE ONLY: [`holder`]/[`holds`] work per venue, so the
/// projection shows both holders armed and the second to mount comes up paper here. No two
/// declarations share a resource today.
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
    Some(ExclusiveClaim { claimed, resource: resource.to_string(), keep: false })
}

#[path = "exclusive_tests.rs"]
#[cfg(test)]
mod exclusive_tests;

#[path = "exclusive_fold_tests.rs"]
#[cfg(test)]
mod exclusive_fold_tests;
