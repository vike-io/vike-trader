//! OTO/OCO book mechanics: the expire cascade to held children and the OCO cancel on a fill.

use vike_model::events::{Event, OrderCanceled};

use super::PaperExecutionClient;

impl PaperExecutionClient {
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
    pub(super) fn expire_children_of(&mut self, parent_coid: &str, ts: i64) {
        let mut queue: Vec<String> = vec![parent_coid.to_string()];
        while let Some(parent) = queue.pop() {
            let children = self.contingency.children_of(&parent);
            for child in children {
                self.contingency.remove(&child);
                self.expiry.shift_remove(&child);
                let before = self.pending.len();
                self.pending.retain(|(c, _)| c != &child);
                if self.pending.len() != before {
                    self.events.push_back(Event::OrderCanceled(OrderCanceled {
                        client_order_id: child.clone(),
                        reason: "parent-expired".to_string().into(),
                        ts,
                    }));
                }
                queue.push(child);
            }
        }
    }

    /// OTO arm-on-fill + OCO cancel-sibling, applied after `filled_coid` fills. A no-op for plain
    /// orders (no contingency entry). The DECISION — which held children arm, which linked
    /// siblings cancel — is the shared [`vike_exec::ContingencyBook::on_fill`] resolver (ONE law
    /// with the backtest SimBroker's lanes, trigger-law wave 2); this method keeps the book
    /// mechanics: cancel each returned sibling still resting here and emit its `OrderCanceled`.
    pub(super) fn apply_contingency(&mut self, filled_coid: &str, ts: i64) {
        for sib in self.contingency.on_fill(filled_coid) {
            if self.pending.iter().any(|(coid, _)| coid == &sib) {
                self.pending.retain(|(coid, _)| coid != &sib);
                self.contingency.remove(&sib);
                self.expiry.shift_remove(&sib);
                self.events.push_back(Event::OrderCanceled(OrderCanceled {
                    client_order_id: sib,
                    reason: "oco".to_string().into(),
                    ts,
                }));
            } else if self.combos.iter().any(|(coid, _)| coid == &sib) {
                // An OCO sibling that is a resting COMBO cancels as ONE order too (all legs at
                // once), same as `cancel`. Only reachable from a PLAIN order's fill during the
                // single-symbol pass — a combo can never carry links itself (`submit` rejects
                // them), so this never runs inside `fill_combos`' mem::take window. Marks are NOT
                // dropped here: `fill_combos` runs right after this pass and owns that hygiene.
                self.combos.retain(|(coid, _)| coid != &sib);
                self.contingency.remove(&sib);
                self.expiry.shift_remove(&sib);
                self.events.push_back(Event::OrderCanceled(OrderCanceled {
                    client_order_id: sib,
                    reason: "oco".to_string().into(),
                    ts,
                }));
            }
        }
    }
}
