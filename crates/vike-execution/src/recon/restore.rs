//! The restore planner: after a restart, which orders a previous session left behind are
//! STILL RESTING at the venue (adopt them into the engine registry), which are GONE (the owning
//! mount must be told), and which must be left alone. Pure: no I/O, no clock, no threads.
//!
//! Inputs are the [`RestoredOrderRef`]s read from the ownership file, the venue's open-order
//! report and fill report of ONE successful pass for ONE (venue, account) — the
//! [`RestoreScope`]. The caller (vike-core, on the fold thread) joins the plan with its
//! order client id → mount map; this crate knows no mounts.
//!
//! ## Contract
//!
//! - **The planner trusts its caller that the fetch SUCCEEDED.** An empty `orders` slice means
//!   "the venue reports nothing resting" for this pass, so every in-scope ref is ABSENT (see
//!   "Order client id absent" for what absence proves). A failed or partial fetch must never reach this
//!   function; the core only receives successful fetches.
//! - **A ref belongs to a pass only when it can.** `ref.venue` is `None` or the scope's venue and
//!   `ref.account` is `None` or the scope's account; a ref of another venue or account is untouched
//!   (no list), matching order client id or not. A `None` venue or account on the ref never blocks an order client id
//!   match (an old record), but the ABSENCE rule below demands exact equality.
//! - **Order client id on the venue.** The report's status is the venue parser's mapping (a word the parser
//!   could not map arrives as `Accepted`, i.e. live, with a warning from the bridge):
//!   non-terminal => `adopt`; FILLED => `filled_while_down`; any other terminal (CANCELED,
//!   REJECTED, EXPIRED, DENIED) => `gone`. A terminal report is PROOF the order is closed, so it
//!   makes the ref gone regardless of [`RestoreScope::symbols`] — but only for a ref carrying
//!   both `venue` and `symbol`, the "full ref" a gone verdict needs (a ref without them stays
//!   untouched, as it is when absent).
//! - **Fill in the pass window.** A ref with a [`FillReport`] naming its order client id OR its venue order id
//!   ([`RestoredOrderRef::venue_order_id`], the id the venue acknowledged; for a fill of the same
//!   symbol), and no LIVE report, is `filled_while_down`: neither adopted nor gone — the existing
//!   `MissingFill` path owns it. A venue's fill rows often carry no order client id (binance, aster, cTrader,
//!   IG, Alpaca, OKX's REST rows), and a triggered stop's fill carries its child order's id, so the
//!   venue id is the join that makes a fill-while-down visible. (A partially filled order canceled
//!   while down lands here too: the fill is the evidence the planner will not overrule; the mount
//!   learns the cancel from the venue's order history on a later pass or not at all.)
//! - **Order client id absent from the venue is UNPROVEN, and only quiet passes make it gone.** Silence is not
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
//!   (`ReconcileClient::lists_trigger_orders`, the kind twin of the symbol claim) as well; otherwise it
//!   stays `unproven` for good: the venue may keep it on a read the client does not make. A ref
//!   that records NO kind (a line written before the terms existed) may be a stop, so it is judged
//!   as a trigger kind: an old plain limit on a client that does not claim stays held until the
//!   ownership file's 30-day TTL drops it.
//! - **A report that names no order client id** is matched here by order client id only; the caller joins it to its restored
//!   order by the acknowledged venue order id FIRST ([`name_by_venue_order_id`], on the same rows its
//!   diff reads), so a venue whose rows carry no id of ours (fxcm, IG) is planned like any other.
//! - **Venue orders the file does not know** are external and appear nowhere in the plan.
//! - Each ref lands in AT MOST ONE list; output is sorted by order client id (deterministic); an order client id
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
    /// (`ReconcileClient::lists_trigger_orders`, carried by `ReconcileReports::lists_trigger_orders`).
    /// `false` (the default and every client's answer unless it proves otherwise) HOLDS an absent
    /// trigger-kind ref instead of declaring it gone.
    pub lists_trigger_orders: bool,
}

/// The verdicts of [`plan_restore`]; every list is sorted by order client id and the lists are disjoint.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RestorePlan {
    /// Venue reports of restored orders still resting: feed them to
    /// `Engine::reregister_orders`. Non-terminal only.
    pub adopt: Vec<OrderStatusReport>,
    /// Restored orders the venue no longer has: the mount gets a synthetic `OrderCanceled`
    /// ([`RESTORE_GONE_REASON`]).
    pub gone: Vec<RestoredOrderRef>,
    /// Order client ids that filled while the process was down: neither adopted nor gone.
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
    // One report per order client id; a LIVE report outranks a terminal duplicate (a venue history that
    // lists a replaced order's old terminal row beside its live successor).
    let mut by_order_client_id: BTreeMap<&str, (&OrderStatusReport, OrderStatus)> = BTreeMap::new();
    for r in orders {
        let Some(order_client_id) = r.order_client_id.as_deref() else { continue };
        let status = r.status;
        match by_order_client_id.get(order_client_id) {
            Some((_, kept)) if !kept.is_terminal() => {}
            Some(_) if status.is_terminal() => {}
            _ => {
                by_order_client_id.insert(order_client_id, (r, status));
            }
        }
    }
    let filled: BTreeSet<&str> =
        fills.iter().filter_map(|f| f.order_client_id.as_deref()).collect();
    // venue order id -> the symbols fills of it were reported on: a fill row that carries no order client id
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

    // Sorted by order client id, one verdict per order client id (the first ref of a repeated order client id wins).
    let mut refs: Vec<&RestoredOrderRef> = restored.iter().collect();
    refs.sort_by(|a, b| a.order_client_id.cmp(&b.order_client_id));
    refs.dedup_by(|b, a| a.order_client_id == b.order_client_id);

    let mut plan = RestorePlan::default();
    for r in refs {
        let venue_ok = r.venue.as_deref().is_none_or(|v| v == scope.venue);
        let account_ok = r.account.is_none() || r.account == scope.account;
        if !venue_ok || !account_ok {
            continue; // another venue's or account's order: not this pass's business
        }
        let has_fill = filled.contains(r.order_client_id.as_str()) || joined_by_id(r);
        match by_order_client_id.get(r.order_client_id.as_str()) {
            Some((report, status)) if !status.is_terminal() => plan.adopt.push((*report).clone()),
            Some(_) if has_fill => plan.filled_while_down.push(r.order_client_id.clone()),
            Some((_, OrderStatus::Filled)) => {
                plan.filled_while_down.push(r.order_client_id.clone())
            }
            // A terminal report is proof, but a gone verdict needs a FULL ref.
            Some(_) if r.venue.is_some() && r.symbol.is_some() => plan.gone.push(r.clone()),
            Some(_) => {}
            None if has_fill => plan.filled_while_down.push(r.order_client_id.clone()),
            None if in_scope(scope, r) && silence_is_proof(scope, r) => plan.gone.push(r.clone()),
            None if in_scope(scope, r) => plan.unproven.push(r.clone()),
            None => {}
        }
    }
    plan
}

/// **An open-order report that names no order client id, joined to its restored order by the venue order id**: the order twin of
/// the fill join in [`plan_restore`], run by the caller BEFORE the planner on the same rows it then diffs. A venue whose
/// rows carry no id of ours (fxcm's Orders table, IG's working orders) lists a resting order as `order_client_id: None`
/// once BOTH daemons restarted, because the bridge that named it by its venue id died too. A report with no order client id whose
/// venue order id is the one a restored ref learned at the acknowledgement ([`RestoredOrderRef::venue_order_id`]), on
/// the ref's symbol, gets the ref's order client id IN PLACE: the planner then adopts it under our order client id, the caller's diff reads it
/// as ours (no `UnknownOrder` beside an `OrphanLocalOrder`), and the client is taught the order with its venue id.
///
/// Applicability is the planner's: a ref of another venue or account is not this pass's. Nothing is guessed: a venue id
/// two restored orders claim names neither, a ref whose order client id a report already names (a bridge that labels its rows) is
/// not joined again, a different symbol is another order, and a report that already names an order client id keeps it. A trigger
/// kind records no venue id on its line, so a stop is never joined this way. Returns how many reports it named.
pub fn name_by_venue_order_id(
    scope: &RestoreScope,
    restored: &[RestoredOrderRef],
    orders: &mut [OrderStatusReport],
) -> usize {
    let named: BTreeSet<String> = orders.iter().filter_map(|o| o.order_client_id.clone()).collect();
    // venue order id -> the one ref that claims it (`None`: two refs claim it, so it names neither)
    let mut by_id: BTreeMap<&str, Option<&RestoredOrderRef>> = BTreeMap::new();
    for r in restored {
        let applies = r.venue.as_deref().is_none_or(|v| v == scope.venue)
            && (r.account.is_none() || r.account == scope.account);
        let Some(id) = r.venue_order_id.as_deref().filter(|id| !id.is_empty()) else { continue };
        if !applies || named.contains(&r.order_client_id) {
            continue;
        }
        by_id
            .entry(id)
            .and_modify(|claim| {
                if claim.is_some_and(|c| c.order_client_id != r.order_client_id) {
                    *claim = None;
                }
            })
            .or_insert(Some(r));
    }
    let mut joined = 0;
    for o in orders.iter_mut().filter(|o| o.order_client_id.is_none()) {
        let Some(Some(r)) = by_id.get(o.venue_order_id.as_str()) else { continue };
        if r.symbol.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(&o.symbol)) {
            o.order_client_id = Some(r.order_client_id.clone());
            joined += 1;
        }
    }
    joined
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
