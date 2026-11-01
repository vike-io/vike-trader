//! `Budget` — the daemon's ONE ledger of lines, and the snapshot of it a status verb will serialise.
//!
//! It is what the venue call sites (the feeds of T5, the REST fetches of T6) hold: an
//! `Arc<Budget>` they call [`Budget::spend`] on before a request goes out. In wave 1 the answer is
//! always [`Admission::Admit`] ([`MODE`]) and the call exists to COUNT and DISCLOSE: the line
//! beneath it warns once per window when a request would have been refused, and
//! [`Budget::snapshot`] exposes every counter.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use serde::Serialize;
use vike_venues::Market;

use super::ceiling::{Axis, ceilings_for, host_label};
use super::class::Class;
use super::key::BudgetKey;
use super::line::{Line, LineSnapshot};
use super::mode::{Admission, MODE, Mode};

/// Every counter the ledger holds, at one instant. Sorted (the ledger is a `BTreeMap`), so two
/// snapshots of the same state serialise identically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    /// The mode this daemon runs ([`MODE`]).
    pub mode: Mode,
    /// Requests on an axis the venue's row declares no ceiling for: counted here and judged
    /// against nothing, so an operator sees traffic the budget cannot speak to.
    pub unmetered: u64,
    /// One entry per `(key, axis)` that has been spent on.
    pub lines: Vec<LineSnapshot>,
}

/// What the ledger holds behind its lock.
#[derive(Debug, Default)]
struct Ledger {
    lines: BTreeMap<(BudgetKey, Axis), Line>,
    unmetered: u64,
}

/// The ledger. Cheap to share (`Arc<Budget>`); every method takes `&self`.
#[derive(Debug, Default)]
pub struct Budget {
    ledger: Mutex<Ledger>,
}

impl Budget {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Spend `cost` units of `class` on `axis` of `venue`'s `market`, at `now`.
    ///
    /// The line is created on first use from the venue's table row. An axis the row declares no
    /// ceiling for (or a venue the table does not know) is counted in [`Snapshot::unmetered`] and
    /// admitted: there is no number to judge it against, and none is invented.
    ///
    /// Logs one `warn!` per line per window when the request is over its split
    /// ([`super::line::Observation::announce`]). The lock is held for the arithmetic only.
    pub fn spend(
        &self,
        venue: &str,
        market: Market,
        axis: Axis,
        class: Class,
        cost: u64,
        now: Instant,
    ) -> Admission {
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
                    return Admission::Admit;
                };
                slot.insert(Line::new(ceiling))
            }
        };
        let observed = line.spend(class, cost, now, MODE);
        if observed.announce {
            tracing::warn!(
                budget = %key,
                axis = axis.as_str(),
                class = class.as_str(),
                admitted = line.ceiling().admitted,
                cost,
                shadow = MODE == Mode::Shadow,
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
    pub fn snapshot(&self, now: Instant) -> Snapshot {
        let mut guard = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let ledger = &mut *guard;
        let lines =
            ledger.lines.iter_mut().map(|((key, _), line)| line.snapshot(key, now)).collect();
        Snapshot { mode: MODE, unmetered: ledger.unmetered, lines }
    }
}
