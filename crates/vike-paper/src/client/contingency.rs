//! OTO/OCO/OUO book mechanics: the expire cascade to held children, the OCO cancel on a fill and
//! the OUO resize in place (a sibling reduced to nothing is cancelled, `ouo`).

use vike_model::events::{Event, OrderCanceled, OrderModified};

use super::ExecutionClientPaper;

impl ExecutionClientPaper {
    /// Cascade-cancel the held OTO children of an order that just EXPIRED (transitively, so a
    /// child-of-a-child goes too).
    ///
    /// A held exit only ever arms on its parent's FILL (`apply_contingency`), so once the parent
    /// has expired its children can never arm and can never fill — left alone they would rest
    /// inactive and invisible-but-live for the rest of the run, inflating the open-order set and
    /// never terminalizing. They are canceled with the distinct reason **`"parent-expired"`** (vs
    /// `"user"` / `"oco"`): the child did not itself reach a deadline, its parent did.
    ///
    /// Only reachable from `expire_due`, i.e. only when an expiring-TIF order was resting, so the
    /// `Gtc` path never runs it.
    pub(super) fn expire_children_of(&mut self, parent_order_client_id: &str, ts: i64) {
        let mut queue: Vec<String> = vec![parent_order_client_id.to_string()];
        while let Some(parent) = queue.pop() {
            let children = self.contingency.children_of(&parent);
            for child in children {
                self.contingency.remove(&child);
                self.expiry.shift_remove(&child);
                let before = self.pending.len();
                self.pending.retain(|(c, _)| c != &child);
                if self.pending.len() != before {
                    self.events.push_back(Event::OrderCanceled(OrderCanceled {
                        order_client_id: child.clone(),
                        reason: "parent-expired".to_string().into(),
                        timestamp: ts,
                        post_only_would_cross: false,
                        refusal_id: None,
                    }));
                }
                queue.push(child);
            }
        }
    }

    /// OTO arm-on-fill, OCO cancel-sibling and OUO reduce-sibling, applied after `filled_order_client_id` fills
    /// `filled_qty` at `fill_price`. A no-op for plain orders (no contingency entry). The DECISION —
    /// which held children arm, which linked siblings cancel or shrink, and by how much — is the
    /// shared [`vike_execution::ContingencyBook::on_fill`] resolver and [`vike_execution::spend_ouo_reduce`]
    /// (ONE law with the core and the backtest OrderBrokerSimulator); this method keeps the book mechanics:
    /// cancel each returned sibling still resting here and emit its `OrderCanceled`, and resize each
    /// reduced one IN PLACE with an `OrderModified` carrying its new size, or cancel it (`ouo`) when
    /// nothing is left.
    pub(super) fn apply_contingency(
        &mut self,
        filled_order_client_id: &str,
        filled_qty: f64,
        fill_price: f64,
        ts: i64,
    ) {
        let resolution = self.contingency.on_fill(filled_order_client_id, filled_qty);
        if !resolution.reduce.is_empty() {
            let shares = self.contingency.ouo_shares_its_fill(filled_order_client_id);
            self.reduce_ouo_siblings(&resolution.reduce, shares, filled_qty, fill_price, ts);
        }
        for sib in resolution.cancel {
            if self.pending.iter().any(|(order_client_id, _)| order_client_id == &sib) {
                self.pending.retain(|(order_client_id, _)| order_client_id != &sib);
                self.contingency.remove(&sib);
                self.expiry.shift_remove(&sib);
                self.events.push_back(Event::OrderCanceled(OrderCanceled {
                    order_client_id: sib,
                    reason: "oco".to_string().into(),
                    timestamp: ts,
                    post_only_would_cross: false,
                    refusal_id: None,
                }));
            } else if self.combos.iter().any(|(order_client_id, _)| order_client_id == &sib) {
                // An OCO sibling that is a resting COMBO cancels as ONE order too (all legs at
                // once), same as `cancel`. Only reachable from a PLAIN order's fill during the
                // single-symbol pass — a combo can never carry links itself (`submit` rejects
                // them), so this never runs inside `fill_combos`' mem::take window. Marks are NOT
                // dropped here: `fill_combos` runs right after this pass and owns that hygiene.
                self.combos.retain(|(order_client_id, _)| order_client_id != &sib);
                self.contingency.remove(&sib);
                self.expiry.shift_remove(&sib);
                self.events.push_back(Event::OrderCanceled(OrderCanceled {
                    order_client_id: sib,
                    reason: "oco".to_string().into(),
                    timestamp: ts,
                    post_only_would_cross: false,
                    refusal_id: None,
                }));
            }
        }
    }

    /// The OUO resize of the `reduce` siblings still resting in `pending`. When the filled leg
    /// `shares` its fill (a ladder's stop: `vike_execution::ContingencyBook::ouo_shares_its_fill`),
    /// `filled_qty` comes off them IN TOTAL, the farthest from `fill_price` first
    /// ([`vike_execution::spend_ouo_reduce`]); any other OUO fill takes `filled_qty` off EACH (a ladder's
    /// target off its one sibling, the stop). A sibling not resting here (filled, cancelled, or in
    /// this bar's fill pass, which `on_bar`'s `resolve_in_pass` decides) takes nothing.
    fn reduce_ouo_siblings(
        &mut self,
        reduce: &[(String, f64)],
        shares: bool,
        filled_qty: f64,
        fill_price: f64,
        ts: i64,
    ) {
        let resting: Vec<(String, f64, f64)> = reduce
            .iter()
            .filter_map(|(sib, _)| {
                let (_, o) = self.pending.iter().find(|(c, _)| c == sib)?;
                Some((sib.clone(), o.price.unwrap_or(fill_price), o.size))
            })
            .collect();
        let reduced: Vec<(String, Option<f64>)> = if shares {
            vike_execution::spend_ouo_reduce(filled_qty, fill_price, resting)
        } else {
            let each = resting.into_iter();
            each.map(|(sib, _, qty)| (sib, vike_execution::reduced_quantity(qty, filled_qty)))
                .collect()
        };
        for (sib, left) in reduced {
            match left {
                Some(q) => {
                    if let Some((_, o)) = self.pending.iter_mut().find(|(c, _)| *c == sib) {
                        o.size = q;
                    }
                    self.events.push_back(Event::OrderModified(OrderModified {
                        venue_order_id: Some(sib.clone().into()),
                        order_client_id: sib,
                        new_qty: Some(q),
                        new_price: None,
                        new_trigger_price: None,
                        timestamp: ts,
                    }));
                }
                None => {
                    self.pending.retain(|(c, _)| *c != sib);
                    self.contingency.remove(&sib);
                    self.expiry.shift_remove(&sib);
                    self.events.push_back(Event::OrderCanceled(OrderCanceled {
                        order_client_id: sib,
                        reason: "ouo".to_string().into(),
                        timestamp: ts,
                        post_only_would_cross: false,
                        refusal_id: None,
                    }));
                }
            }
        }
    }
}
