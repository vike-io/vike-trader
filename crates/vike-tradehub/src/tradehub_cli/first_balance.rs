//! **The boot's reconcile driver puts the venue's balance on the engines BEFORE the door opens** (decision 0246).
//! COLD: one blocking wait on the starting thread, never a message, and nothing in the fold.
//!
//! The control door opens as soon as the live mount returns, a couple of seconds before the reconcile driver's first
//! pass does, and until a leg's first pass is on the core's lane its engine judges orders on `seed_cash`: wrong in
//! both directions (a rich account refused for margin it has, a poor one admitted against equity it does not).
//! [`mount_reconcile_driver`] closes that window for the boot's legs the way a hot start closes it for its own:
//!
//! 1. every leg gets a cell its pass fills ([`vike_tradehub_core::await_first_balances`]) and the door is told
//!    ([`LeaseFacts::engine_awaiting_balance`]): a leg whose balance has not landed refuses its account's OPENING
//!    orders BY NAME, with the leg's last reason in words, whatever else happens;
//! 2. the driver is spawned, and the start waits up to the bound for the cells (it stops waiting for an account a pass
//!    failed on with no fast retry coming: the bound is for a venue that says nothing)
//!    ([`vike_tradehub_core::wait_for_first_balances`]): the balance lands as the reconcile pass's own
//!    authoritative balance write (`vike_core`'s `reconcile_reports`), the only one there is; this module writes
//!    nothing, cancels nothing and places nothing, so a restart under `quarantine` keeps its resting orders;
//! 3. an account the venue did not answer in time (a failed read, a leg that is not ready, a stuck fetch) stays
//!    refused for OPENING orders with the reason while every other account trades; reducing orders, cancels, a
//!    flatten and a market exit are never held.
//!
//! The driver visits its legs one after the other on its own thread (blocking fetches, off the fold), so one slow
//! leg delays the first read of the legs behind it, never the door beyond the bound.

use std::sync::mpsc::{Receiver, Sender};

use super::*;
use crate::remote_backend::facts::LeaseFacts;
use vike_tradehub_core::{ReconConfig, ReconDriver, ReconLeg};

/// What the boot's wait may report to and be ended by.
pub(super) struct BootWait<'a> {
    /// The `starting` door's progress record, told what the start is waiting for (`None`: nowhere).
    pub(super) progress: Option<&'a crate::server::starting::StartProgress>,
    /// The process's stop flag; a raised flag ends the wait at once (`None`: watches nothing).
    pub(super) stop: Option<&'a AtomicBool>,
    /// How long the start waits for the cells before the door opens anyway.
    pub(super) bound: Duration,
}

/// **Mount the boot's reconcile driver, and hold the door's opening until every leg's venue balance has reached
/// its engine or `wait.bound` has passed** (module doc). Returns the driver as `spawn_recon` does.
pub(super) fn mount_reconcile_driver(
    handle: &CoreHandle,
    mut clients: Vec<ReconLeg>,
    config: ReconConfig,
    trigger: Option<(Sender<()>, Receiver<()>)>,
    facts: Option<&LeaseFacts>,
    wait: &BootWait<'_>,
) -> ReconDriver {
    let waits = vike_tradehub_core::await_first_balances(&mut clients);
    if let Some(facts) = facts {
        for (route_key, cell) in &waits {
            facts.engine_awaiting_balance(route_key, cell);
        }
    }
    let driver = vike_tradehub_core::spawn_recon(handle, clients, config, trigger);
    if waits.is_empty() {
        return driver;
    }
    if let Some(progress) = wait.progress {
        progress.phase("waiting for the venue balances of the accounts");
    }
    let unanswered = vike_tradehub_core::wait_for_first_balances(&waits, wait.bound, || {
        driver.is_finished() || wait.stop.is_some_and(|stop| stop.load(Ordering::Relaxed))
    });
    if unanswered.is_empty() {
        tracing::info!(
            accounts = waits.len(),
            "boot: every account's venue balance reached its engine before the door opens"
        );
    } else {
        tracing::warn!(
            unanswered = ?unanswered,
            bound_s = wait.bound.as_secs(),
            "boot: the venue balance of these accounts has not reached their engines; the door \
             refuses their OPENING orders, by name and with the reason, until the reconcile pass \
             answers (the other accounts trade). Reducing orders, cancels and exits are not held"
        );
    }
    driver
}
