//! `Budget` — the daemon's ONE ledger of lines, and the snapshot of it `Request::GatewayStatus` reports.
//!
//! It is what the trade dispatch holds (`crate::trade::dispatch`'s `spend`, before an order-class command goes out),
//! and what the connection chain's READS spend (`super::reads`: the bridges' history replays and the reconcile
//! fetches): an `Arc<Budget>` they call [`Budget::spend`] on. The answer is always [`BudgetAdmission::Admit`]
//! ([`MODE`]) and the call exists to COUNT and DISCLOSE: the line beneath it warns once per window when a request
//! would have been refused, and [`Budget::snapshot`] exposes every counter.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use serde::Serialize;
use vike_venues::VenueRateLimitsMarket;

use super::ceiling::{BudgetCeilingAxis, ceilings_for, host_label};
use super::class::BudgetClass;
use super::key::BudgetKey;
use super::line::{BudgetLine, BudgetLineSnapshot};
use super::mode::{BudgetAdmission, BudgetMode, MODE};

/// Every counter the ledger holds, at one instant. Sorted (the ledger is a `BTreeMap`), so two
/// snapshots of the same state serialise identically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BudgetSnapshot {
    /// The mode this ledger runs ([`MODE`] in every daemon).
    pub mode: BudgetMode,
    /// Requests on an axis the venue's row declares no ceiling for: counted here and judged
    /// against nothing, so an operator sees traffic the budget cannot speak to.
    pub unmetered: u64,
    /// One entry per `(key, axis)` that has been spent on.
    pub lines: Vec<BudgetLineSnapshot>,
}

/// What the ledger holds behind its lock.
#[derive(Debug, Default)]
struct Ledger {
    lines: BTreeMap<(BudgetKey, BudgetCeilingAxis), BudgetLine>,
    unmetered: u64,
}

/// The ledger. Cheap to share (`Arc<Budget>`); every method takes `&self`.
#[derive(Debug)]
pub struct Budget {
    ledger: Mutex<Ledger>,
    /// [`MODE`] in every daemon; [`Budget::enforcing`] exists for the tests of what a refusal does.
    mode: BudgetMode,
}

impl Default for Budget {
    fn default() -> Self {
        Self::new()
    }
}

impl Budget {
    /// An empty ledger, in this build's [`MODE`].
    #[must_use]
    pub fn new() -> Self {
        Self { ledger: Mutex::default(), mode: MODE }
    }

    /// An empty ledger that REFUSES what is over its split ([`BudgetMode::Enforce`]): only for a test of what a
    /// refusal does downstream, never built into a daemon (`budget_tests.rs`'s `wave_one_refuses_nothing` pins [`MODE`]).
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn enforcing() -> Self {
        Self { ledger: Mutex::default(), mode: BudgetMode::Enforce }
    }

    /// Spend `cost` units of `class` on `axis` of `venue`'s `market`, at `now`.
    ///
    /// The line is created on first use from the venue's table row. An axis the row declares no
    /// ceiling for (or a venue the table does not know) is counted in [`BudgetSnapshot::unmetered`] and
    /// admitted: there is no number to judge it against, and none is invented.
    ///
    /// Logs one `warn!` per line per window when the request is over its split
    /// ([`super::line::BudgetObservation::announce`]). The lock is held for the arithmetic only.
    pub fn spend(
        &self,
        venue: &str,
        market: VenueRateLimitsMarket,
        axis: BudgetCeilingAxis,
        class: BudgetClass,
        cost: u64,
        now: Instant,
    ) -> BudgetAdmission {
        let key = BudgetKey::new(venue, host_label(venue, market), axis.scope());
        let mut guard = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let ledger = &mut *guard;
        let line = match ledger.lines.entry((key.clone(), axis)) {
            Entry::Occupied(slot) => slot.into_mut(),
            Entry::Vacant(slot) => {
                let Some(ceiling) =
                    ceilings_for(venue, market).into_iter().find(|c| c.axis == axis)
                else {
                    ledger.unmetered = ledger.unmetered.saturating_add(1);
                    return BudgetAdmission::Admit;
                };
                slot.insert(BudgetLine::new(ceiling))
            }
        };
        let observed = line.spend(class, cost, now, self.mode);
        if observed.announce && observed.admission == BudgetAdmission::Refuse {
            tracing::info!(
                budget = %key,
                axis = axis.as_str(),
                class = class.as_str(),
                admitted = line.ceiling().admitted,
                cost,
                "vike-gatewayhub budget: refused a request — the {} budget for {} is over its split. A refused read \
                 is skipped and the next step of its chain covers it. This is the only line for this budget until \
                 its window rolls over",
                axis.as_str(),
                key
            );
        } else if observed.announce {
            tracing::warn!(
                budget = %key,
                axis = axis.as_str(),
                class = class.as_str(),
                admitted = line.ceiling().admitted,
                cost,
                shadow = self.mode == BudgetMode::Shadow,
                "vike-gatewayhub budget: this request would have been throttled — the {} budget \
                 for {} is over its split. Counted and admitted: wave 1 refuses nothing. This is \
                 the only warning for this budget until its window rolls over",
                axis.as_str(),
                key
            );
        }
        observed.admission
    }

    /// Every line's counters as of `now`.
    #[must_use]
    pub fn snapshot(&self, now: Instant) -> BudgetSnapshot {
        let mut guard = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let ledger = &mut *guard;
        let lines =
            ledger.lines.iter_mut().map(|((key, _), line)| line.snapshot(key, now)).collect();
        BudgetSnapshot { mode: self.mode, unmetered: ledger.unmetered, lines }
    }
}
