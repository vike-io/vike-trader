//! ORDER RESTORE at the reconcile pass: after a restart, the orders a previous session left
//! resting go back into the engine registry, and the mounts that own them hear what became of them.
//!
//! The hook is [`CoreThread::restore_orders_at_pass`], called once per pass from
//! `crates/vike-core/src/runtime/reconcile.rs`'s `reconcile_reports`, AFTER the pass's engine is
//! resolved and BEFORE `reconcile_compute` diffs the venue's reports against the registry, so an
//! order adopted here is a known local order to that diff and raises no `UnknownOrder`.
//!
//! # What it reads
//!
//! - [`CoreThread::restored_orders`], the orders the ownership file remembers for a MOUNTED slot,
//!   and the coids of [`CoreThread::pending_owners`], the ones whose mount is not mounted yet.
//! - This pass's open-order and fill reports, as the fold received them in
//!   [`ReconcileReports`]. A FAILED fetch never reaches this code: in
//!   `crates/vike-tradehub-core/src/recon_manager/manager.rs`'s `run_pass_at` the `fetch_mass_status` error arm
//!   `continue`s and sends no command,
//!   so every pass seen here is a successful fetch and the planner's "the venue reports nothing"
//!   reading of an empty list is sound. No per-pass state is needed for that.
//!
//! The verdicts are `vike_exec::recon::plan_restore`'s, whose module doc holds every rule; this file
//! only supplies its scope and applies its plan.
//!
//! # The translation step (a venue whose reports do not echo our coid)
//!
//! Hyperliquid names an order on the wire by `keccak(coid)`, and a restart empties the bridge's
//! cloid registry, so its reports carry a hash this crate cannot invert and cannot compute (vike-core
//! sits below the bridge, and the bridge may not read the ownership file). The core therefore asks
//! the pass's client, per restored entry, `ExecutionClient::wire_id_for(coid)`; for the entries that
//! answer `Some`, it rewrites THIS pass's order and fill reports in place (wire id back to coid, and
//! the venue-native symbol back to the entry's unified symbol) before planning, and the same
//! rewritten vectors go on to `reconcile_compute`, so the existing `MissingFill` path books a fill
//! under the real coid and `mount_for_coid` finds its mount. A client with the default `None` makes
//! the map empty and the pass reads exactly as it did. After the adoption the client is told
//! `(coid, unified symbol)` through `ExecutionClient::adopt_restored`, so its own wire state (the
//! Hyperliquid cloid registry and its coid -> asset map) matches what the session that placed the
//! order would hold. ⚠ Only restored entries are translated, and only on a pass that has some: a
//! later pass reads the venue's wire ids as the venue sent them (decision 0121 names the residual).
//!
//! # The scope of a pass
//!
//! One pass reads one (venue, account). The scope's venue is the pass's venue; its account is the
//! engine's account LABEL TEXT (`label_of_route_key` of the pass's route key: `None` for the venue's
//! default account) and its symbols are the symbols the engine of this route serves (`symbol` plus
//! `extra_symbols`), never every mount's. That is the same spelling `OwnedOrder::account` was
//! written in at mint (`CoreThread::minted_facts`: the mount's label text, `None` for the default
//! account), which is what lets the planner compare the two with `==`. An entry for another account,
//! another venue, a symbol this engine does not serve or without venue and symbol facts is not
//! judged: it stays for a later pass.
//!
//! # A symbol the client was not told about is HELD (decision 0174)
//!
//! The venue's silence about an order is only an answer for a symbol the pass's client FETCHED. The
//! engine serves its mounted symbol, its declared legs and the symbols admitted on demand
//! (decision 0173), but a reconcile client reads an admitted symbol only after it was told the engine
//! admitted it, and it may not have been (the lease could not answer the symbol's facts at boot, or the
//! gatewayhub restarted alone and its new daemon knows nothing). So when the pass carries a list
//! (`ReconcileReports::admitted_symbols`), an ON-DEMAND symbol of the engine that is not on it is
//! taken out of the scope: its restored orders are neither adopted from a report that cannot list
//! them nor declared gone, they stay in `restored_orders` and in the file, and ONE note per order
//! (`CoreThread::restore_held`, an edge set) says so; the first pass whose client has been told judges
//! them. A report that does list the order still adopts it, and a terminal one still proves it
//! closed. The mounted symbol and a declared leg are judged either way, and a pass with no list
//! (`None`: a client that keeps none, a journal written before the field) judges the engine's symbols
//! as it always did. The list is journaled with the pass, so a replay takes the same verdicts.
//!
//! # What each verdict does
//!
//! - **ADOPT** (the coid is still resting at the venue). The report goes into the engine registry
//!   through `ExecutionEngine::reregister_orders` (insert-only, no event, no journal record), with
//!   the limit price the JOURNAL restored for the entry when it has one (a venue report carries
//!   none; decision 0127 amends decision 0120's verdict 8 for a box with a journal), the
//!   order's `coid_venue` row is set for a non-primary engine, and the owning mount is sent a
//!   synthetic tagged `OrderAccepted` through the ONE lane real order events use: it is pushed on
//!   the engine's `order_events` buffer, which `dispatch_applied_fills` delivers right after the
//!   command, so the tag stamp, the per-mount routing and the `on_order_event` call are exactly a
//!   real event's. It is a liveness confirmation (a restored `SpreadMaker` side stays unverified
//!   until a tagged non-terminal event or a fill arrives) and nothing else: it reaches neither the
//!   journal nor the account/registry fold. The entry leaves `restored_orders`, and only AFTER the
//!   registry holds the order.
//! - **GONE** (absent at the venue, or the venue reports it closed, and no fill in the window). The
//!   mount is sent a synthetic `Canceled { reason: RESTORE_GONE_REASON }` through the same lane,
//!   which retires the restored tag (`retire_tag_for_coid`) and stamps it on the event; the
//!   ownership entry is forgotten in the file at once (`OwnerRecord::Forget`; the linger prune
//!   writes a harmless second one) and leaves `restored_orders`.
//! - **FILLED WHILE DOWN** (a fill report in the window, or the venue reports it filled). Neither
//!   adopted nor gone: the pass's existing `MissingFill` path owns the fill. The entry leaves
//!   `restored_orders` so a later pass cannot count it again; the file entry, `coid_mount` and the
//!   tag stay, and the ordinary terminal path or the time-to-live retires them.
//!
//! # A mount that is not mounted yet
//!
//! An entry in `pending_owners` belongs to a mount id no slot carries. It is judged like the others,
//! with two differences, both signed by the owner: an ADOPTED order goes into the registry (so the
//! diff does not call it external) and nobody is told, and a GONE order is only forgotten (file and
//! pending table, plus the note) because there is no mount to tell. The pending entry of an adopted
//! order stays, so that when a mount of that id lands it binds the order's tag and ownership as
//! always, and the NEXT pass confirms the order to the new mount (it is then in `restored_orders`
//! and already in the registry: delivered, not counted again). A pending entry that filled while
//! down is left alone and uncounted: removing it would lose the bind, and counting it each pass
//! would repeat.
//!
//! # Cost
//!
//! Pass cadence, never the per-message fold. A pass for a venue with nothing restored, with the venue
//! gate off or with the operator switch on costs two emptiness checks and a function-pointer call.
//! With entries, one pass builds one ref per entry and the plan; the mount delivery then walks the
//! tag registry once per event (`tag_for_coid` scans it), which is a restart-time cost bounded by
//! the orders a previous session left resting.

use super::*;
use std::collections::HashMap;
use vike_exec::execution_engine::OrderEventOut;
use vike_exec::recon::{
    RESTORE_GONE_REASON, RestorePlan, RestoreScope, RestoredOrderRef, plan_restore,
};
use vike_model::accounts::account_keys::label_of_route_key;
use vike_model::{FillReport, OrderEventKind, OrderLifecycle, OrderStatusReport};

/// What one applied plan did, for the counters and the note.
#[derive(Default)]
struct Applied {
    /// Orders newly written into the registry.
    adopted: u64,
    /// Mount deliveries queued: a mount told its restored order is alive.
    confirmed: u64,
    /// GONE verdicts.
    gone: u64,
    /// Of those, the ones whose mount is not mounted (nobody told).
    gone_unmounted: u64,
    /// FILLED-WHILE-DOWN entries newly counted.
    filled: u64,
    /// The coids this pass acted on.
    touched: Vec<String>,
}

impl<C: ExecutionClient> CoreThread<C> {
    /// The restore hook of one reconcile pass; the module doc is the contract. `idx` is the pass's
    /// engine, `venue` the exchange it read and `account_key` the route key of its account (the
    /// venue itself for a venue's sole account).
    pub(super) fn restore_orders_at_pass(
        &mut self,
        idx: usize,
        venue: &str,
        account_key: &str,
        orders: &mut [OrderStatusReport],
        fills: &mut [FillReport],
        admitted: Option<&[String]>,
    ) {
        if self.config.restore_orders_off {
            return;
        }
        let pending = self.pending_owners.values().any(|p| !p.coids.is_empty());
        if self.restored_orders.is_empty() && !pending {
            return;
        }
        if !(self.config.restore_venue_gate)(venue) {
            return;
        }
        // An engine whose key is not a legal account of this venue cannot be named in a scope.
        let Some(label) = label_of_route_key(venue, account_key) else {
            return;
        };
        // The symbols the engine admitted on demand that this pass's client was NOT told about: it
        // did not fetch them, so the venue's silence about them is not an answer (see "The scope of
        // a pass"). Empty unless the pass carries a list.
        let unread: Vec<String> = admitted.map_or_else(Vec::new, |told| {
            let e = self.eng(idx);
            e.extra_symbols
                .iter()
                .filter(|s| {
                    e.gate.limits.grid_by_symbol.get(s.as_str()).is_some_and(|g| g.on_demand)
                        && !told.contains(s)
                })
                .cloned()
                .collect()
        });
        let scope = {
            let e = self.eng(idx);
            RestoreScope {
                venue: venue.to_string(),
                account: label.text().map(str::to_string),
                symbols: std::iter::once(e.symbol.clone())
                    .chain(e.extra_symbols.iter().cloned())
                    .filter(|s| !unread.contains(s))
                    .collect(),
            }
        };
        let refs = self.restore_refs();
        // The translation step (decision 0121): a client whose reports name an order by a function
        // of its coid rewrites this pass's reports back to the coid BEFORE the planner reads them,
        // and the caller's diff reads the same rewritten rows.
        let wire = self.wire_ids_of_refs(idx, &scope, &refs);
        if !wire.is_empty() {
            translate_wire_ids(&wire, orders, fills);
        }
        let plan = plan_restore(&scope, &refs, orders, fills);
        self.hold_unread(venue, account_key, &scope, &unread, &refs, &plan);
        self.apply_restore_plan(idx, venue, account_key, &plan);
    }

    /// Say, once per order, which restored orders this pass could not judge because their symbol was
    /// not read: an admitted symbol whose facts the lease could not answer, or a gatewayhub that
    /// restarted and has not been told yet. They stay in `restored_orders` (and in the file), so the
    /// first pass whose client has been told judges them. A coid that is no longer held (adopted,
    /// gone, or its symbol read again) leaves the edge set, so a later hold is announced again.
    fn hold_unread(
        &mut self,
        venue: &str,
        account_key: &str,
        scope: &RestoreScope,
        unread: &[String],
        refs: &[RestoredOrderRef],
        plan: &RestorePlan,
    ) {
        let spent = |coid: &str| {
            plan.adopt.iter().any(|r| r.client_order_id.as_deref() == Some(coid))
                || plan.gone.iter().any(|r| r.coid == coid)
                || plan.filled_while_down.iter().any(|c| c == coid)
        };
        let mut held: Vec<String> = refs
            .iter()
            .filter(|r| {
                r.venue.as_deref().is_none_or(|v| v == scope.venue)
                    && (r.account.is_none() || r.account == scope.account)
                    && r.symbol.as_ref().is_some_and(|s| unread.contains(s))
                    && !spent(&r.coid)
            })
            .map(|r| r.coid.clone())
            .collect();
        held.sort_unstable();
        held.dedup();
        self.restore_held.retain(|c| held.contains(c));
        let fresh: Vec<String> =
            held.into_iter().filter(|c| self.restore_held.insert(c.clone())).collect();
        if fresh.is_empty() {
            return;
        }
        let note = format!(
            "RESTORE {venue} [account {account_key}]: HELD {} order(s) on admitted symbol(s) {} the \
             reconcile client was not told about: not adopted, not declared gone, judged by the \
             first pass whose client has been told: {}",
            fresh.len(),
            unread.join(", "),
            super::boot_restore::sample(&fresh),
        );
        tracing::warn!(target: "vike_core::reconcile", "{note}");
        self.note(note);
    }

    /// `wire id -> (coid, unified symbol)` of the restored entries this pass may judge, for a client
    /// whose [`ExecutionClient::wire_id_for`] names them. Empty for every client with the default
    /// (`None`), so a venue that echoes our coid pays one trait call per entry and nothing else.
    /// Applicability is the planner's own: a ref of another venue or account is not this pass's.
    fn wire_ids_of_refs(
        &self,
        idx: usize,
        scope: &RestoreScope,
        refs: &[RestoredOrderRef],
    ) -> HashMap<String, (String, Option<String>)> {
        let client = &self.eng(idx).client;
        refs.iter()
            .filter(|r| {
                r.venue.as_deref().is_none_or(|v| v == scope.venue)
                    && (r.account.is_none() || r.account == scope.account)
            })
            .filter_map(|r| {
                let wire = client.wire_id_for(&r.coid)?;
                Some((wire, (r.coid.clone(), r.symbol.clone())))
            })
            .collect()
    }

    /// One planner ref per restored entry: the mounted ones first, then the pending ones (an entry
    /// of a mount id no slot carries yet), each coid once.
    fn restore_refs(&self) -> Vec<RestoredOrderRef> {
        let to_ref = |o: &crate::order_owners::OwnedOrder| RestoredOrderRef {
            coid: o.coid.clone(),
            venue: o.venue.clone(),
            symbol: o.symbol.clone(),
            account: o.account.clone(),
        };
        let mut refs: Vec<RestoredOrderRef> = self.restored_orders.values().map(to_ref).collect();
        for p in self.pending_owners.values() {
            refs.extend(
                p.coids.iter().filter(|o| !self.restored_orders.contains_key(&o.coid)).map(to_ref),
            );
        }
        refs
    }

    /// Apply `plan`; see the module doc for each verdict.
    fn apply_restore_plan(
        &mut self,
        idx: usize,
        venue: &str,
        account_key: &str,
        plan: &RestorePlan,
    ) {
        let mut done = Applied::default();
        self.restore_adopt(idx, plan, &mut done);
        self.restore_gone(idx, plan, &mut done);
        self.restore_filled(plan, &mut done);

        self.restore_counters.restored_adopted += done.adopted;
        self.restore_counters.restored_gone += done.gone;
        self.restore_counters.restored_filled_while_down += done.filled;
        if done.adopted + done.confirmed + done.gone + done.filled == 0 {
            return; // a repeat pass: every verdict was already applied
        }
        done.touched.sort_unstable();
        done.touched.dedup();
        self.note(format!(
            "RESTORE {venue} [account {account_key}]: adopted {} (mounts told {}), gone {} \
             ({} of unmounted mounts, no mount told), filled while down {}: {}",
            done.adopted,
            done.confirmed,
            done.gone,
            done.gone_unmounted,
            done.filled,
            super::boot_restore::sample(&done.touched),
        ));
    }

    /// ADOPT: registry write, engine row, and the owning mount's liveness confirmation.
    fn restore_adopt(&mut self, idx: usize, plan: &RestorePlan, done: &mut Applied) {
        if plan.adopt.is_empty() {
            return;
        }
        let fresh: Vec<OrderStatusReport> = plan
            .adopt
            .iter()
            .filter(|r| {
                r.client_order_id
                    .as_deref()
                    .is_some_and(|c| !self.eng(idx).registry.contains_key(c))
            })
            .cloned()
            .collect();
        // Tell the client BEFORE the registry write, so the first WS event or fill of a restored
        // order it receives already resolves (a client with no restart-lost state ignores this).
        let announced: Vec<(String, String)> = fresh
            .iter()
            .filter_map(|r| Some((r.client_order_id.clone()?, r.symbol.clone())))
            .collect();
        if !announced.is_empty() {
            self.eng_mut(idx).client.adopt_restored(&announced);
        }
        // The JOURNALED limit price of each adopted order (decision 0127, closing decision 0120's
        // verdict 8 where a journal is on): a venue report carries none, so the rebuilt registry
        // entry would rest at no price. Read before the entries are spent below; the venue still
        // decides that the order exists, the journal only says where it rested.
        let prices: Vec<(String, f64)> = announced
            .iter()
            .filter_map(|(coid, _)| {
                let entry = self.restored_orders.get(coid).or_else(|| {
                    self.pending_owners
                        .values()
                        .flat_map(|p| p.coids.iter())
                        .find(|o| o.coid == *coid)
                })?;
                Some((coid.clone(), entry.price?))
            })
            .collect();
        done.adopted = self.eng_mut(idx).reregister_orders(&fresh) as u64;
        for (coid, px) in prices {
            if let Some(order) = self.eng_mut(idx).registry.get_mut(&coid)
                && order.request.price.is_none()
            {
                order.request.price = Some(px);
            }
        }
        for r in &plan.adopt {
            let Some(coid) = r.client_order_id.as_deref() else { continue };
            if fresh.iter().any(|f| f.client_order_id.as_deref() == Some(coid)) {
                done.touched.push(coid.to_string());
            }
            // The order's own engine, the row a submit would have written, so its lifecycle events
            // route to the engine that holds it and not to the primary.
            if idx != 0 {
                self.coid_venue.insert(coid.to_string(), idx);
            }
            // Only the entry of a MOUNTED slot is spent (and only now that the registry holds the
            // order); a pending entry stays, for its mount to bind.
            if self.restored_orders.shift_remove(coid).is_some() {
                if self.mount_for_coid(coid).is_some() {
                    self.queue_order_event(
                        idx,
                        &r.venue,
                        &r.symbol,
                        coid,
                        OrderEventKind::Accepted,
                    );
                    done.confirmed += 1;
                }
                done.touched.push(coid.to_string());
            }
        }
    }

    /// GONE: forget the entry in the file and the tables, and tell the mount when there is one.
    fn restore_gone(&mut self, idx: usize, plan: &RestorePlan, done: &mut Applied) {
        for r in &plan.gone {
            done.gone += 1;
            done.touched.push(r.coid.clone());
            self.record_owner(crate::order_owners::OwnerRecord::Forget { coid: r.coid.clone() });
            if self.restored_orders.shift_remove(&r.coid).is_some() {
                if let (Some(_), Some(venue), Some(symbol)) =
                    (self.mount_for_coid(&r.coid), &r.venue, &r.symbol)
                {
                    let kind = OrderEventKind::Canceled { reason: RESTORE_GONE_REASON.to_string() };
                    self.queue_order_event(idx, venue, symbol, &r.coid, kind);
                } else {
                    done.gone_unmounted += 1;
                }
            } else {
                done.gone_unmounted += 1;
                self.pending_owners.retain(|_, p| {
                    p.coids.retain(|o| o.coid != r.coid);
                    !p.coids.is_empty() || p.ledger.is_some()
                });
            }
        }
    }

    /// FILLED WHILE DOWN: counted once, off the planner's set; the fill itself is the existing
    /// `MissingFill` path's.
    fn restore_filled(&mut self, plan: &RestorePlan, done: &mut Applied) {
        for coid in &plan.filled_while_down {
            if self.restored_orders.shift_remove(coid).is_some() {
                done.filled += 1;
                done.touched.push(coid.clone());
            }
        }
    }

    /// Put one synthetic lifecycle event on `idx`'s engine buffer, the lane
    /// `crates/vike-core/src/runtime/strategy_drive/order_events.rs`'s `dispatch_order_events`
    /// drains: the tag is left `None` here because that lane stamps it (`tag_for_coid` for a
    /// non-terminal kind, `retire_tag_for_coid` for a terminal one).
    pub(super) fn queue_order_event(
        &mut self,
        idx: usize,
        venue: &str,
        symbol: &str,
        coid: &str,
        kind: OrderEventKind,
    ) {
        self.eng_mut(idx).order_events.push(OrderEventOut {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            event: OrderLifecycle { client_order_id: coid.to_string(), tag: None, kind },
        });
    }
}

/// Rewrite the pass's reports from a venue wire id back to the restored coid it names, in place:
/// `client_order_id` becomes the coid and, for an order report or a fill, `symbol` becomes the
/// UNIFIED symbol the ownership record holds (a venue-native spelling such as a spot coin never
/// reaches the registry for a translated order). Rows naming no restored order are untouched.
fn translate_wire_ids(
    wire: &HashMap<String, (String, Option<String>)>,
    orders: &mut [OrderStatusReport],
    fills: &mut [FillReport],
) {
    for o in orders {
        if let Some((coid, symbol)) = o.client_order_id.as_deref().and_then(|w| wire.get(w)) {
            let (coid, symbol) = (coid.clone(), symbol.clone());
            o.client_order_id = Some(coid);
            if let Some(s) = symbol {
                o.symbol = s;
            }
        }
    }
    for f in fills {
        if let Some((coid, symbol)) = f.client_order_id.as_deref().and_then(|w| wire.get(w)) {
            let (coid, symbol) = (coid.clone(), symbol.clone());
            f.client_order_id = Some(coid);
            if let Some(s) = symbol {
                f.symbol = s;
            }
        }
    }
}
