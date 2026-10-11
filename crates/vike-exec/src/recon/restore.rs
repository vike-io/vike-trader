//! The restore planner: after a restart, which orders a previous session left behind are
//! STILL RESTING at the venue (adopt them into the engine registry), which are GONE (the owning
//! mount must be told), and which must be left alone. Pure: no I/O, no clock, no threads.
//!
//! Inputs are the [`RestoredOrderRef`]s read from the ownership file, the venue's open-order
//! report and fill report of ONE successful pass for ONE (venue, account) — the
//! [`RestoreScope`]. The caller (vike-core, on the fold thread) joins the plan with its
//! coid → mount map; this crate knows no mounts.
//!
//! ## Contract
//!
//! - **The planner trusts its caller that the fetch SUCCEEDED.** An empty `orders` slice means
//!   "the venue reports nothing resting", so every in-scope ref is declared gone. A failed or
//!   partial fetch must never reach this function; the core only receives successful fetches.
//! - **A ref belongs to a pass only when it can.** `ref.venue` is `None` or the scope's venue and
//!   `ref.account` is `None` or the scope's account; a ref of another venue or account is untouched
//!   (no list), matching coid or not. A `None` venue or account on the ref never blocks a coid
//!   match (an old record), but the ABSENCE rule below demands exact equality.
//! - **Coid on the venue.** The report is parsed with `OrderStatus::parse` (an unparsable status
//!   counts as live, like `ExecutionEngine::reregister_orders`):
//!   non-terminal => `adopt`; FILLED => `filled_while_down`; any other terminal (CANCELED,
//!   REJECTED, EXPIRED, DENIED) => `gone`. A terminal report is PROOF the order is closed, so it
//!   makes the ref gone regardless of [`RestoreScope::symbols`] — but only for a ref carrying
//!   both `venue` and `symbol`, the "full ref" a gone verdict needs (a ref without them stays
//!   untouched, as it is when absent).
//! - **Fill in the pass window.** A ref with a [`FillReport`] naming its coid, and no LIVE
//!   report, is `filled_while_down`: neither adopted nor gone — the existing `MissingFill` path
//!   owns it. (A partially filled order canceled while down lands here too: the fill is the
//!   evidence the planner will not overrule; the mount learns the cancel from the venue's
//!   order history on a later pass or not at all.)
//! - **Coid absent from the venue.** `gone` only when the ref is IN SCOPE: `venue` equals the
//!   scope's, `symbol` is `Some` and listed in [`RestoreScope::symbols`] (the symbols the
//!   reporting client COVERS — a symbol-scoped fetch says nothing about the rest), and `account`
//!   equals the scope's. Otherwise untouched: no list.
//! - **Venue orders the file does not know** are external and appear nowhere in the plan.
//! - Each ref lands in AT MOST ONE list; output is sorted by coid (deterministic); a coid
//!   repeated in `restored` is judged once.
//! - `adopt` holds only non-terminal reports.

use std::collections::{BTreeMap, BTreeSet};

use vike_model::{FillReport, OrderStatusReport};

use super::types::RestoredOrderRef;
use crate::order::OrderStatus;

/// The reason text of the synthetic `OrderCanceled` the core delivers to the mount of a
/// [`RestorePlan::gone`] order. Lives here so the core and its tests spell it once.
pub const RESTORE_GONE_REASON: &str = "restart: absent at venue";

/// What ONE reporting client covers: the (venue, account) a pass ran for and the symbols its
/// fetch spans. The caller derives `symbols` from the engines the client serves. An empty
/// `symbols` declares nothing gone on absence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreScope {
    pub venue: String,
    pub account: Option<String>,
    pub symbols: Vec<String>,
}

/// The verdicts of [`plan_restore`]; every list is sorted by coid and the lists are disjoint.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RestorePlan {
    /// Venue reports of restored orders still resting: feed them to
    /// `ExecutionEngine::reregister_orders`. Non-terminal only.
    pub adopt: Vec<OrderStatusReport>,
    /// Restored orders the venue no longer has: the mount gets a synthetic `OrderCanceled`
    /// ([`RESTORE_GONE_REASON`]).
    pub gone: Vec<RestoredOrderRef>,
    /// Coids that filled while the process was down: neither adopted nor gone.
    pub filled_while_down: Vec<String>,
}

/// Plan the restore of `restored` against one successful pass (`orders`, `fills`) of `scope`.
/// Pure; see the module doc for every rule.
pub fn plan_restore(
    scope: &RestoreScope,
    restored: &[RestoredOrderRef],
    orders: &[OrderStatusReport],
    fills: &[FillReport],
) -> RestorePlan {
    // One report per coid; a LIVE report outranks a terminal duplicate (a venue history that
    // lists a replaced order's old terminal row beside its live successor).
    let mut by_coid: BTreeMap<&str, (&OrderStatusReport, OrderStatus)> = BTreeMap::new();
    for r in orders {
        let Some(coid) = r.client_order_id.as_deref() else { continue };
        let status = OrderStatus::parse(&r.status).unwrap_or(OrderStatus::Accepted);
        match by_coid.get(coid) {
            Some((_, kept)) if !kept.is_terminal() => {}
            Some(_) if status.is_terminal() => {}
            _ => {
                by_coid.insert(coid, (r, status));
            }
        }
    }
    let filled: BTreeSet<&str> =
        fills.iter().filter_map(|f| f.client_order_id.as_deref()).collect();

    // Sorted by coid, one verdict per coid (the first ref of a repeated coid wins).
    let mut refs: Vec<&RestoredOrderRef> = restored.iter().collect();
    refs.sort_by(|a, b| a.coid.cmp(&b.coid));
    refs.dedup_by(|b, a| a.coid == b.coid);

    let mut plan = RestorePlan::default();
    for r in refs {
        let venue_ok = r.venue.as_deref().is_none_or(|v| v == scope.venue);
        let account_ok = r.account.is_none() || r.account == scope.account;
        if !venue_ok || !account_ok {
            continue; // another venue's or account's order: not this pass's business
        }
        let has_fill = filled.contains(r.coid.as_str());
        match by_coid.get(r.coid.as_str()) {
            Some((report, status)) if !status.is_terminal() => plan.adopt.push((*report).clone()),
            Some(_) if has_fill => plan.filled_while_down.push(r.coid.clone()),
            Some((_, OrderStatus::Filled)) => plan.filled_while_down.push(r.coid.clone()),
            // A terminal report is proof, but a gone verdict needs a FULL ref.
            Some(_) if r.venue.is_some() && r.symbol.is_some() => plan.gone.push(r.clone()),
            Some(_) => {}
            None if has_fill => plan.filled_while_down.push(r.coid.clone()),
            None if in_scope(scope, r) => plan.gone.push(r.clone()),
            None => {}
        }
    }
    plan
}

/// The absence rule: only a ref the reporting client demonstrably COVERS may be declared gone by
/// the venue's silence — same venue, a listed symbol, exactly the same account.
fn in_scope(scope: &RestoreScope, r: &RestoredOrderRef) -> bool {
    r.venue.as_deref() == Some(scope.venue.as_str())
        && r.symbol.as_deref().is_some_and(|s| scope.symbols.iter().any(|c| c == s))
        && r.account == scope.account
}

#[path = "restore_tests.rs"]
#[cfg(test)]
mod restore_tests;
