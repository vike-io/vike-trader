//! **A resource only ONE account of a venue may hold per process** — which account holds it, and
//! the claim. Generic over `VenueDeclaration::process_exclusive`; dukascopy's JForex sidecar is the
//! one such resource today — `DukascopyVenueMount` declares it — and
//! `crates/bridges/dukascopy/src/mount.rs`'s module doc carries the evidence and the measurement
//! that would retire the limit. The claim is RAII: a guard that is not kept is released, because a
//! failed start must not refuse the next account with a reason that is not true.
//!
//! The claim is keyed on the RESOURCE (`ProcessExclusive::resource`), not on the venue: two venues
//! that declare one resource exclude each other, which is what sharing a resource means. The
//! legacy dukascopy arm claimed under `crate::dukascopy::SIDECAR_RESOURCE`, the name its port
//! declares, so the key did not change when the arm moved into the bridge.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use vike_model::accounts::account_keys::AccountLabel;

use crate::{MountPolicy, VenueRow};

/// Which of the armed LABELLED accounts holds the resource: the first in label order, or the
/// DEFAULT account when none is armed. Was `crate::dukascopy::sidecar_holder`, whose doc (the
/// rule, and why the default account yields) moved here with it.
///
/// `armed` is every LABELLED account whose arming row is above paper on its own merits — the
/// operator armed it, the store identifies it, and its credentials are present — in label order.
/// The rule:
///
/// * **none** → [`AccountLabel::Default`]. That is every deployment that exists today, and it is
///   byte-identical to the first-come claim that preceded this function: the default account mounts
///   `DUKASCOPY_DEMO1_*`.
/// * **exactly one** → that account, and **the DEFAULT account declines**.
/// * **more than one** → the FIRST in label order, and the rest decline.
///
/// # ⚠ Why the DEFAULT account declines, which is the whole point
///
/// The fan-out mounts the default account FIRST ([`crate::accounts_to_mount`] preserves row order
/// and [`crate::arming::known_accounts`] puts `Default` first), and `account_ceiling` is
/// `min(venue line, account line)` — so raising `policy.venues.dukascopy` to `demo`, which is the
/// only way to get a `policy.accounts.dukascopy.<LABEL>` row above paper, NECESSARILY arms the
/// default account too. With a first-come claim the default therefore took the sidecar on every
/// box, every time, and **there was no policy that could mount the second account**. The feature
/// shipped unreachable.
///
/// So giving an account a `policy.accounts.dukascopy.<LABEL>` row is read as what it plainly is —
/// *this is the dukascopy account I want this process to trade* — and the account that was never
/// named yields to the one that was. The alternative (default wins, labelled accounts refused) is
/// what shipped and is the defect; the other alternative (refuse BOTH and strand the venue)
/// punishes a configuration the operator states clearly.
///
/// ⚠ **The cost, stated rather than discovered**: on a box that arms a labelled dukascopy account, a
/// strategy mounted on the venue's DEFAULT account now runs on a PAPER engine where it used to trade.
/// It is loud — the arm logs an `error!` naming the holder, and the arming row carries
/// `vike_config::ArmingBlock::SidecarHeldElsewhere` with its own sentence (`VenueArming::why`, the
/// Venues tab's hover text) — but `crates/vike-mount/src/node.rs`'s `refuse_unarmed_mount_accounts` does NOT refuse it,
/// because that refusal deliberately skips mounts that name no account. An operator who wants the
/// default account back deletes that `policy.accounts.dukascopy.<LABEL>` row.
///
/// # More than one is a PICK, and picking is legitimate here
///
/// Everywhere else in the dukascopy resolution a guess would choose a LEGAL ENTITY for somebody —
/// see `crates/bridges/dukascopy/src/account.rs`'s `DukascopyRefusal` — and every such arm refuses.
/// This one does not choose an entity: each armed account still resolves to its own row's own key
/// family, and what is being rationed is a PROCESS RESOURCE. Refusing every account instead would
/// take a working box off the venue over a line that names a real account. And the account that
/// loses is not silently
/// mistraded: it resolves PAPER, the projection says so, and a strategy that names it is refused
/// outright by `crates/vike-mount/src/node.rs`'s `refuse_unarmed_mount_accounts`.
pub(crate) fn pick_holder(armed: &[AccountLabel]) -> AccountLabel {
    armed.first().cloned().unwrap_or(AccountLabel::Default)
}

/// Which account of `venue` holds this process's exclusive resource, from the POLICY. Was
/// `crate::arming::dukascopy_sidecar_holder`.
///
/// The candidate set is every LABELLED account of the venue this box knows about whose arming would
/// stand on its own merits — `crate::arming`'s `account_arming_raw`, i.e. the exclusive-resource
/// question deliberately NOT asked, which is what makes this terminate. An account the operator
/// named but that cannot arm (no `policy.accounts` row above paper, no credentials, a store that
/// cannot identify it) takes nothing from the default account: a box mid-migration that writes a
/// row it cannot yet satisfy keeps mounting the account it has always mounted, rather than losing
/// it to an account that will not arm either.
///
/// What it costs, stated rather than hidden: one store-key parse plus one `account_arming_raw` per
/// LABELLED account of the venue, recomputed per row rather than hoisted. Bounded by the number of
/// accounts an operator has named on the venue, and pure arithmetic over maps this caller already
/// holds, since the `account` table arrives as DATA.
pub(crate) fn holder(
    registry: &'static [VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> AccountLabel {
    // No policy is no `policy.accounts` rows, so nothing can have been named and the DEFAULT account
    // holds it — the answer this venue has always given.
    let Some(p) = policy else { return AccountLabel::Default };
    let armed: Vec<AccountLabel> = crate::arming::known_accounts(venue, vars, Some(p))
        .into_iter()
        .filter(|l| !l.is_default())
        .filter(|l| {
            crate::arming::account_arming_raw(
                registry,
                venue,
                l,
                vars,
                p.venues.account(venue, l),
                Some(p),
            )
            .0 != vike_config::VenueMode::Paper
        })
        .collect();
    pick_holder(&armed)
}

/// Whether `label` holds `venue`'s resource. Was `crate::arming::dukascopy_holds_sidecar`.
pub(crate) fn holds(
    registry: &'static [VenueRow],
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> bool {
    *label == holder(registry, venue, vars, policy)
}

/// The resources this process has claimed, by `ProcessExclusive::resource`. Was
/// `crate::dukascopy::SIDECAR`, a flag for one venue.
static CLAIMED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A held claim, RELEASED on drop unless [`ExclusiveClaim::keep`] commits it. Was `SidecarClaim`.
///
/// ⚠ **The release is the point, and its absence was a live defect.** The claim was a bare flag
/// that nothing ever cleared, so a FAILED start — for dukascopy an absent jar, a `Command::spawn`
/// error, a `Fatal` envelope from a bad login, or its `READY_TIMEOUT` elapsing — burned it, and
/// every later account in the process was then refused with *this process already runs one* when it
/// runs none. RAII rather than a `release()` call on the error arm: a `return` added between the
/// claim and the start would silently reintroduce the leak, and `Drop` cannot be forgotten.
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

/// The process-level backstop: `Some` for the first caller per resource. Was `claim_sidecar`.
///
/// It should never fire on a correctly-resolved mount — [`holder`] names exactly one account per
/// process and the mount refuses every other one before reaching here. What it still covers is a
/// process that mounts the venue TWICE (two cores in one process), which no policy describes and no
/// projection can predict — and two venues that declare the same resource.
///
/// ⚠ That second case is excluded HERE ONLY. [`holder`] and [`holds`] work per venue, so the
/// projection shows both venues' holders armed, and the second to mount comes up paper through this
/// backstop. No two declarations share a resource today.
#[must_use]
pub(crate) fn claim(resource: &str) -> Option<ExclusiveClaim<'static>> {
    claim_in(&CLAIMED, resource)
}

/// [`claim`] over a caller-supplied set — the test seam.
///
/// ⚠ A POISONED set is recovered, not read as "held". The set is a plain list of names, so a panic
/// on another thread while it held the lock cannot leave it half-written, and answering `None`
/// would refuse this account with *already claimed* — a reason that is not true.
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
