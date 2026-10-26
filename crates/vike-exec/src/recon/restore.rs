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
//!   "the venue reports nothing resting" for this pass, so every in-scope ref is ABSENT (see
//!   "Coid absent" for what absence proves). A failed or partial fetch must never reach this
//!   function; the core only receives successful fetches.
//! - **A ref belongs to a pass only when it can.** `ref.venue` is `None` or the scope's venue and
//!   `ref.account` is `None` or the scope's account; a ref of another venue or account is untouched
//!   (no list), matching coid or not. A `None` venue or account on the ref never blocks a coid
//!   match (an old record), but the ABSENCE rule below demands exact equality.
//! - **Coid on the venue.** The report's status is the venue parser's mapping (a word the parser
//!   could not map arrives as `Accepted`, i.e. live, with a warning from the bridge):
//!   non-terminal => `adopt`; FILLED => `filled_while_down`; any other terminal (CANCELED,
//!   REJECTED, EXPIRED, DENIED) => `gone`. A terminal report is PROOF the order is closed, so it
//!   makes the ref gone regardless of [`RestoreScope::symbols`] — but only for a ref carrying
//!   both `venue` and `symbol`, the "full ref" a gone verdict needs (a ref without them stays
//!   untouched, as it is when absent).
//! - **Fill in the pass window.** A ref with a [`FillReport`] naming its coid OR its venue order id
//!   ([`RestoredOrderRef::venue_order_id`], the id the venue acknowledged; for a fill of the same
//!   symbol), and no LIVE report, is `filled_while_down`: neither adopted nor gone — the existing
//!   `MissingFill` path owns it. A venue's fill rows often carry no coid (binance, aster, cTrader,
//!   IG, Alpaca, OKX's REST rows), and a triggered stop's fill carries its child order's id, so the
//!   venue id is the join that makes a fill-while-down visible. (A partially filled order canceled
//!   while down lands here too: the fill is the evidence the planner will not overrule; the mount
//!   learns the cancel from the venue's order history on a later pass or not at all.)
//! - **Coid absent from the venue is UNPROVEN, and only quiet passes make it gone.** Silence is not
//!   proof of absence: a page the read did not cover, a kind it does not list, a fill whose row
//!   carries no id the ref knows. So an in-scope absent ref (`venue` equals the scope's, `symbol`
//!   is `Some` and listed in [`RestoreScope::symbols`] — the symbols the reporting client COVERS, a
//!   symbol-scoped fetch says nothing about the rest — and `account` equals the scope's) lands in
//!   `unproven`, which the caller keeps (no mount told, the file keeps the entry) and counts: it
//!   becomes `gone` only when [`RestoredOrderRef::quiet_passes`] has reached
//!   [`RESTORE_QUIET_PASSES`] (the caller counts the in-scope silent passes whose fill window
//!   opened after the first one, so the lookback in which a fill could have shown has passed). Not
//!   in scope: untouched, no list.
//! - **Silence about a KIND is not an answer unless the client claims to list it.** Even then
//!   only after the quiet passes: an absent ref whose recorded kind is a TRIGGER kind (stop,
//!   stop-limit, take-profit) is declared gone only when [`RestoreScope::lists_trigger_orders`]
//!   (`ReconClient::lists_trigger_orders`, the kind twin of the symbol claim) as well; otherwise it
//!   stays `unproven` for good: the venue may keep it on a read the client does not make. A ref
//!   that records NO kind (a line written before the terms existed) may be a stop, so it is judged
//!   as a trigger kind: an old plain limit on a client that does not claim stays held until the
//!   ownership file's 30-day TTL drops it.
//! - **Venue orders the file does not know** are external and appear nowhere in the plan.
//! - Each ref lands in AT MOST ONE list; output is sorted by coid (deterministic); a coid
//!   repeated in `restored` is judged once.
//! - `adopt` holds only non-terminal reports.

use std::collections::{BTreeMap, BTreeSet};

use vike_model::{FillReport, OrderStatusReport};

use super::types::RestoredOrderRef;
use vike_model::OrderStatus;

/// The reason text of the synthetic `OrderCanceled` the core delivers to the mount of a
/// [`RestorePlan::gone`] order. Lives here so the core and its tests spell it once.
pub const RESTORE_GONE_REASON: &str = "restart: absent at venue";

/// How many QUIET passes make an absent ref gone: the pass reads the venue's silence about the ref
/// ([`RestoredOrderRef::quiet_passes`] counts them, the caller keeps the count), and each counted
/// pass is one whose fill window opened after the first silent one, so the lookback in which a fill
/// of the order could have shown has passed N times over. Three: one pass can be a short read, two
/// agree by luck, and the first counted pass already cost a whole lookback (the daemon's default is
/// an hour). It is a floor, never a deadline: an order that is not gone stays held and noted.
pub const RESTORE_QUIET_PASSES: u32 = 3;

/// The shortest lookback (ms of wall clock) a pass's fill window is READ as when it is counted as
/// quiet: ten minutes. The window is `reconcile_lookback_ms` long and an operator can set it to one
/// millisecond; counted at face value, three passes seconds apart would then prove an order gone on
/// a window that saw nothing. The caller (vike-core's `restore_refs`) counts a bounded window only
/// once this much time has gone by since the first silent pass.
pub const RESTORE_MIN_QUIET_SPAN_MS: i64 = 600_000;

/// What ONE reporting client covers: the (venue, account) a pass ran for and the symbols its
/// fetch spans. The caller derives `symbols` from the engines the client serves. An empty
/// `symbols` declares nothing gone on absence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreScope {
    pub venue: String,
    pub account: Option<String>,
    pub symbols: Vec<String>,
    /// The pass's client CLAIMS its open-order read lists trigger kinds
    /// (`ReconClient::lists_trigger_orders`, carried by `ReconcileReports::lists_trigger_orders`).
    /// `false` (the default and every client's answer unless it proves otherwise) HOLDS an absent
    /// trigger-kind ref instead of declaring it gone.
    pub lists_trigger_orders: bool,
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
    /// In-scope refs the report does not list and whose silence is NO PROOF (see the module doc):
    /// neither adopted nor gone, kept by the caller, no mount told.
    pub unproven: Vec<RestoredOrderRef>,
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
        let status = r.status;
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
    // venue order id -> the symbols fills of it were reported on: a fill row that carries no coid
    // joins its order by the id the venue acknowledged.
    let mut filled_by_id: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for f in fills.iter().filter(|f| !f.venue_order_id.is_empty()) {
        filled_by_id.entry(&f.venue_order_id).or_default().push(&f.symbol);
    }
    let joined_by_id = |r: &RestoredOrderRef| -> bool {
        r.venue_order_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .and_then(|id| filled_by_id.get(id))
            .is_some_and(|symbols| {
                // The symbol must agree: a ref without one cannot say the fill is its own.
                r.symbol
                    .as_deref()
                    .is_some_and(|s| symbols.iter().any(|f| f.eq_ignore_ascii_case(s)))
            })
    };

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
        let has_fill = filled.contains(r.coid.as_str()) || joined_by_id(r);
        match by_coid.get(r.coid.as_str()) {
            Some((report, status)) if !status.is_terminal() => plan.adopt.push((*report).clone()),
            Some(_) if has_fill => plan.filled_while_down.push(r.coid.clone()),
            Some((_, OrderStatus::Filled)) => plan.filled_while_down.push(r.coid.clone()),
            // A terminal report is proof, but a gone verdict needs a FULL ref.
            Some(_) if r.venue.is_some() && r.symbol.is_some() => plan.gone.push(r.clone()),
            Some(_) => {}
            None if has_fill => plan.filled_while_down.push(r.coid.clone()),
            None if in_scope(scope, r) && silence_is_proof(scope, r) => plan.gone.push(r.clone()),
            None if in_scope(scope, r) => plan.unproven.push(r.clone()),
            None => {}
        }
    }
    plan
}

/// Is the venue's silence about `r` an answer? Only after [`RESTORE_QUIET_PASSES`] counted quiet
/// passes, and, for a restored order that is or MAY BE of a TRIGGER kind (looked for in a read the
/// client may not make), only from a client that claims to list trigger orders. A ref that records
/// no kind (a line written before the terms existed) may be a stop, so it is judged as a trigger
/// kind: on a client that does not claim, an old plain limit stays held until the file's 30-day TTL.
fn silence_is_proof(scope: &RestoreScope, r: &RestoredOrderRef) -> bool {
    r.quiet_passes >= RESTORE_QUIET_PASSES
        && (scope.lists_trigger_orders || r.order_type.as_ref().is_some_and(|t| !t.is_trigger()))
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
