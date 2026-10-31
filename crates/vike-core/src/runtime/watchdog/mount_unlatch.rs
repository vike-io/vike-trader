//! The operator's UN-LATCH of one mount's budget (`Command::UnlatchMount`, `vike-cli trade mount-unlatch`,
//! decision 0239).
//!
//! A mount whose `MountBudget` is breached is latched liquidate-only for good ([`CoreThread::latch_mount`]):
//! its own orders cancelled, its new intents discarded, its flatten offered again at every sweep while it holds
//! a position. Nothing but a restart's re-judgement of an unlatched ledger or an unmount and a mount of the same
//! id cleared it, and a restart put it back (`SnapMountAttr::latched`). This is the operator's way out, and it is
//! deliberately narrow:
//!
//! * **Refused in words, nothing changed**: a mount the node does not run, one that is not latched, one whose
//!   account is stopped, one that still holds a position (the refusal says whether its flatten was offered
//!   again just now, or why not, and that a cancel-only budget is closed by hand) and one that still has a live
//!   order or an armed conditional. The operator unlatches a mount he has seen flat.
//! * **Accepted**: the latch flag is cleared and the loss budget is RE-ARMED from the mount's CURRENT net
//!   realized PnL ([`CoreThread::mount_loss_base`]; the mount is flat, so it has no unrealized). Without a new
//!   base the sweep would judge the cumulative loss that latched the mount and latch it again at the next
//!   interval. The new base is said in the note, never a silent reset; `max_notional` judges the open exposure
//!   and has no history, so it is unchanged.
//! * The account's trading state, every other mount and every venue are untouched. Nothing is written at a
//!   venue.
//!
//! Cold: one command, never the fold. Not journaled as a `Cmd` (`Command::UnlatchMount`'s doc has the argument);
//! it is durable through the `Snap` written at the end, which carries `latched` and `loss_base`.

use super::risk::Reoffer;
use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// The [`Command::UnlatchMount`] arm's body: see the module doc. Every refusal is a recent-events note and a
    /// WARN, never a panic.
    pub(crate) fn unlatch_mount(&mut self, mount_id: &str) {
        let now = self.engine.now_ms;
        if mount_id.trim().is_empty() {
            self.refuse_unlatch("an empty mount id names no mount".to_string());
            return;
        }
        // The identity law `unmount_strategy_runtime` applies: ids are STORED sanitized, so the requested id is
        // sanitized the same way (`maker-a` finds the mount keyed `maker_a`).
        let want = crate::strategy_state::mount_id_with(Some(mount_id), "", "", "");
        let Some(i) =
            (0..self.mounts.len()).find(|&i| self.mounts[i].is_some() && self.mount_ids[i] == want)
        else {
            let live: Vec<&str> = (0..self.mounts.len())
                .filter(|&i| self.mounts[i].is_some())
                .map(|i| self.mount_ids[i].as_str())
                .collect();
            let ids = if live.is_empty() { "it runs none".to_string() } else { live.join(", ") };
            self.refuse_unlatch(format!("no live strategy mount with id `{want}` ({ids})"));
            return;
        };
        let (venue, symbol) = self.mount_vs[i].clone();
        if !self.mount_latched[i] {
            self.refuse_unlatch(format!(
                "mount `{want}` ({venue}/{symbol}) is not latched: there is nothing to clear"
            ));
            return;
        }
        if self.is_retired(self.mount_eng(i)) {
            self.refuse_unlatch(format!(
                "mount `{want}` ({venue}/{symbol}) trades an account that is stopped: nothing runs it, and its \
                 flatten could not be sent; unlatch it once the account runs again"
            ));
            return;
        }
        // (1) A POSITION. The operator unlatches a mount he has seen flat; the refusal says what is closing it.
        let size = self.mount_attr[i].size;
        if size.abs() > 1e-12 {
            let flatten = self.mount_budget[i].is_some_and(|b| b.flatten_on_breach);
            let tail = if flatten {
                match self.reoffer_mount_flatten(i, now) {
                    Reoffer::Sent(q) => format!(
                        "its flatten was offered again just now ({q:.8}) and is offered again at every sweep \
                         until it is flat"
                    ),
                    Reoffer::InFlight => "a reduce-only close is already out for it (the latch's own flatten, \
                                          the margin call's or an operator's) and none was stacked on it"
                        .to_string(),
                    Reoffer::Waits => "its flatten waits: the mount's engine is halted or its trade link is \
                                       down, and it is offered again at a sweep once that is over"
                        .to_string(),
                    Reoffer::Flat => "it reads flat now: send the command again".to_string(),
                }
            } else {
                "this budget is cancel-only (`flatten_on_breach` is off), so nothing closes the position: \
                 close it by hand (`vike-cli trade position flatten`)"
                    .to_string()
            };
            self.refuse_unlatch(format!(
                "mount `{want}` ({venue}/{symbol}) still holds {size:.8}: unlatch it once it is flat; {tail}"
            ));
            return;
        }
        // (2) A LIVE ORDER or an ARMED CONDITIONAL of the mount's own. The latch cancelled its resting orders
        // once and a cancel a venue held back is not offered again, so one can still be out.
        let (live, first) = self.live_orders_of_mount(i);
        let arms = self.cond_engine.values().filter(|a| a.mount == Some(i)).count();
        if live > 0 || arms > 0 {
            let named = first.map(|c| format!(" (for one: `{c}`)")).unwrap_or_default();
            self.refuse_unlatch(format!(
                "mount `{want}` ({venue}/{symbol}) still has {live} live order(s){named} and {arms} armed \
                 conditional(s): the latch cancels the mount's resting orders once, and a cancel the venue held \
                 back is not offered again; cancel them (`vike-cli trade order cancel`, or `mass-cancel`) and \
                 unlatch once the published account shows none"
            ));
            return;
        }
        // (3) CLEAR the latch and RE-ARM the loss budget from where the mount stands. The mount is flat, so
        // its PnL is its realized one: the base the sweep counts its loss from.
        let attr = self.mount_attr[i];
        let realized_net = attr.realized_pnl - attr.fees_paid;
        let base = if realized_net.is_finite() { realized_net } else { 0.0 };
        let was_loss = -(realized_net - self.mount_loss_base[i]);
        self.mount_loss_base[i] = base;
        self.mount_latched[i] = false;
        self.mount_reoffer_said.remove(&i);
        let budget = self.mount_budget[i];
        let loss_arm = match budget.and_then(|b| b.max_loss).filter(|ml| *ml > 0.0) {
            Some(ml) => format!(
                "it counted a loss of {was_loss:.2} when it latched and latches again after a further loss of \
                 {ml:.2}"
            ),
            None => "this budget has no loss arm".to_string(),
        };
        let notional_arm = match budget.and_then(|b| b.max_notional).filter(|mn| *mn > 0.0) {
            Some(mn) => format!(
                "; its notional cap {mn:.2} is unchanged (it judges the open exposure, not history)"
            ),
            None => String::new(),
        };
        tracing::warn!(
            target: "vike_core::risk_clock",
            mount = i,
            mount_id = %want,
            %venue,
            %symbol,
            loss_base = base,
            "OPERATOR UNLATCHED the mount budget: the mount trades again and its loss budget counts from the new base"
        );
        self.note(format!(
            "MOUNT BUDGET UNLATCHED by the operator: mount `{want}` ({venue}/{symbol}) trades again. Its loss \
             budget is re-armed from its CURRENT net realized PnL of {base:.2}, the new base: {loss_arm}{notional_arm}"
        ));
        self.dirty = true;
        // Durable at once: the `Snap`'s mount rows carry `latched` (absent while false) and `loss_base`, so a
        // restart after this puts the mount back UNLATCHED with its new base instead of latching it again on the
        // history it just left. A journal-less core has nothing to write.
        if self.journal.is_some() {
            self.write_snap(now);
        }
    }

    /// `(how many live orders mount `i` owns on any engine, the first one's client order id)`: its attributed
    /// coids (`coid_mount`) the engines' registries still hold live. A terminal or unknown coid counts nothing.
    fn live_orders_of_mount(&self, i: usize) -> (usize, Option<String>) {
        let engines = 1 + self.extra_engines.len();
        let mut live: Vec<&String> = self
            .coid_mount
            .iter()
            .filter(|&(_, &m)| m == i)
            .map(|(coid, _)| coid)
            .filter(|coid| {
                (0..engines).any(|e| {
                    self.eng(e).registry.get(coid.as_str()).is_some_and(|o| o.status.is_live())
                })
            })
            .collect();
        live.sort();
        (live.len(), live.first().map(|c| (*c).clone()))
    }

    /// One refusal of the un-latch: a WARN and a line on the recent-events ring, nothing else.
    fn refuse_unlatch(&mut self, why: String) {
        tracing::warn!(target: "vike_core::risk_clock", %why, "MOUNT UNLATCH REFUSED");
        self.note(format!("MOUNT UNLATCH REFUSED: {why}"));
    }
}
