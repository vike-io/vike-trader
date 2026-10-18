//! Fold-thread FRESHNESS of the held-alert store: what a completed reconcile pass says about the
//! rows already held, and the log line an operator confirm leaves.
//!
//! A held row used to be inserted or refreshed and NEVER removed except by a confirm, so a
//! divergence a later pass no longer raised stayed listed (`trade recon ls`) and confirmable: a
//! pass that omitted `AAPL` held an `OrphanLocalPosition`, the next pass reported `AAPL` correctly,
//! and the confirm still closed the local position while the venue held it. Now each pass stamps
//! the account's [`PassMark`] (a sequence number and what the pass could say about each kind), every
//! row it raises or refreshes carries that sequence, and a row whose kind the pass COVERED but did
//! not raise is stale: [`CoreThread::prune_stale_recon`] removes it, and `confirm_recon` refuses it.
//!
//! ## What a pass can say ([`PassCover::covers`]) - and what it cannot
//! Only a DELIVERED pass reaches the fold (`ReconManager` sends nothing on a failed or not-ready
//! fetch), so a failed pass touches nothing here. A delivered pass is still silent about some kinds:
//! - **positions**: the three position kinds are judged only when the pass carried at least one
//!   position row. `vike_exec::recon::diff` gates its orphan sweep the same way, because an empty
//!   report is indistinguishable from "this venue has no position concept / the fetch is
//!   symbol-scoped and was not asked", so an empty one clears nothing.
//! - **balance**: judged only when `reconcile_balance` is on AND the venue reported a figure.
//! - **journal**: `JournalDivergence` is judged only with a journal view provider set.
//! - **`MissingFill` is never cleared by absence.** A missed fill is a historical venue fact: it
//!   ages out of the reconcile lookback while it stays true and unbooked, and the venue ceasing to
//!   report it is exactly what that looks like. Its confirm is idempotent (the fill ledger refuses a
//!   trade id it has seen), so a stale row costs nothing, and clearing it would destroy the
//!   operator's only route to booking a fill that really happened
//!   (`crates/vike-core/src/runtime/tests/recon_held.rs`'s
//!   `a_missed_fill_the_venue_stops_reporting_keeps_its_row`).
//!   Orders are judged by every delivered pass: an empty order report is the venue's own answer.
//!
//! Cold path only (a reconcile pass, an operator command), never the per-message fold.

use std::collections::HashMap;

use super::*;

/// What one DELIVERED pass could say about the held kinds (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PassCover {
    /// The pass carried at least one position row.
    pub(crate) positions: bool,
    /// `reconcile_balance` was on and the venue reported a balance.
    pub(crate) balance: bool,
    /// A journal view provider was set, so the three-way check ran.
    pub(crate) journal: bool,
}

impl PassCover {
    /// Whether a pass with this cover would have raised `kind` had it still been true. A TOTAL
    /// match, so a new [`DivergenceKind`] is a compile error here rather than a kind that silently
    /// inherits an answer.
    pub(crate) fn covers(self, kind: DivergenceKind) -> bool {
        match kind {
            DivergenceKind::MissingFill => false,
            DivergenceKind::MissingTerminal
            | DivergenceKind::OrphanLocalOrder
            | DivergenceKind::UnknownOrder => true,
            DivergenceKind::PositionDrift
            | DivergenceKind::PositionOnlyExternal
            | DivergenceKind::OrphanLocalPosition => self.positions,
            DivergenceKind::BalanceDrift => self.balance,
            DivergenceKind::JournalDivergence => self.journal,
        }
    }
}

/// The latest delivered pass of ONE account: its sequence number and its [`PassCover`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct PassMark {
    pub(crate) seq: u64,
    pub(crate) cover: PassCover,
}

/// Whether `held` is contradicted by its account's latest pass: that pass covered its kind and did
/// not raise it (a raised row carries the pass's `seq`). `false` for an account with no pass yet.
pub(crate) fn is_stale(marks: &HashMap<String, PassMark>, held: &HeldReconAlert) -> bool {
    marks
        .get(&held.route_key)
        .is_some_and(|m| held.raised_seq != m.seq && m.cover.covers(held.kind))
}

impl<C: ExecutionClient> CoreThread<C> {
    /// Open this account's next pass: bump its sequence and record what it can say. Returns the
    /// sequence the pass's raised rows must carry.
    pub(super) fn open_recon_pass(&mut self, account_key: &str, cover: PassCover) -> u64 {
        let seq = self.recon_pass.get(account_key).map_or(0, |m| m.seq) + 1;
        self.recon_pass.insert(account_key.to_string(), PassMark { seq, cover });
        seq
    }

    /// The confirm door's own re-validation of the row it just took out of the store: `true` (and a
    /// ring note and a WARN, nothing folded) when its account's latest pass contradicts it
    /// ([`is_stale`]). The prune after every pass already removes such rows, so this is the door
    /// checking for itself rather than relying on it.
    pub(super) fn refuse_stale_confirm(&mut self, id: u64, held: &HeldReconAlert) -> bool {
        if !is_stale(&self.recon_pass, held) {
            return false;
        }
        self.note(format!(
            "RECON confirm #{id}: REFUSED, the latest pass no longer raises {:?} (nothing folded) \
             [account {}]",
            held.kind, held.route_key
        ));
        log_confirm(
            true,
            id,
            &held.route_key,
            held.kind,
            "stale: the latest pass no longer raises it",
        );
        true
    }

    /// Remove this account's held rows its latest pass contradicts ([`is_stale`]), one ring note
    /// each. The announcer needs nothing from here: it already reports an identity the pass did not
    /// raise as cleared.
    pub(super) fn prune_stale_recon(&mut self, account_key: &str) {
        let gone: Vec<u64> = self
            .recon_alerts
            .iter()
            .filter(|(_, h)| h.route_key == account_key && is_stale(&self.recon_pass, h))
            .map(|(&id, _)| id)
            .collect();
        for id in gone {
            if let Some(h) = self.recon_alerts.shift_remove(&id) {
                self.note(format!(
                    "RECON cleared #{id} {:?}: the latest pass no longer raises it [account {account_key}]",
                    h.kind
                ));
            }
        }
    }

    /// The fold half of an `OrphanLocalPosition` confirm (and of a flat-unpriced `PositionDrift`
    /// one, which carries the same `OrphanPosition`): drop the LOCAL position the venue never
    /// reported by booking ONE closing `Fill` through the ordinary event fold, and say in one
    /// clause what happened (the confirm note carries it).
    ///
    /// ⚠ **What is booked, exactly.** A fill on the opposite side for the position's whole size, at
    /// the position's OWN `avg_px`, with no commission: `compute_fill` realizes `(px - avg_px) * qty
    /// = 0`, so realized PnL, cash and fees do not move, and no price is invented (the reason the
    /// kind folds nothing under any policy). The one residue is a `0.0` entry in `closed_pnls`
    /// (every closing fill appends its realized PnL), which counts as a flat trade in the stats.
    /// The trade id is minted from the pass clock (`EXT-DROP-…`), so a replayed confirm is the same
    /// fill and a repeat is refused by the fill ledger.
    ///
    /// ⚠ **It refuses what it cannot prove is still the phantom.** The qty must STILL be the one
    /// the pass saw (`o.local_qty`): a strategy fill since then means the row describes a book that
    /// no longer exists, and sweeping the new qty away unseen would be the destructive flatten the
    /// rollout rule exists to prevent. The refusal leaves the position, and the next pass re-raises
    /// the divergence against the real number. The result is verified by reading the book again:
    /// an engine that refuses the symbol (`accepts_symbol`) drops the fill silently.
    pub(super) fn drop_orphan_position(
        &mut self,
        idx: usize,
        o: &vike_exec::recon::OrphanPosition,
    ) -> String {
        let engine = self.eng(idx);
        let venue = engine.venue.clone();
        let key = (
            ustr::Ustr::from(venue.as_str()),
            ustr::Ustr::from(o.symbol.as_str()),
            o.position_side,
        );
        let Some(pos) = engine.account.positions.get(&key).copied().filter(|p| p.size != 0.0)
        else {
            return "the local position is already flat: nothing booked".to_string();
        };
        if pos.size != o.local_qty {
            return format!(
                "NOT dropped: the local {} is now {} where the pass saw {}; the next pass re-raises it",
                o.symbol, pos.size, o.local_qty
            );
        }
        if !(pos.avg_px.is_finite() && pos.avg_px > 0.0) {
            return format!(
                "NOT dropped: the local average price {} is not a usable close price",
                pos.avg_px
            );
        }
        let ts = self.engine.now_ms;
        let side = vike_model::closing_side(pos.size);
        let fill = vike_model::events::FillEvent {
            trade_id: vike_model::events::TradeId::prefixed(
                "EXT-DROP-",
                format_args!("{venue}-{}-{}-{ts}", o.symbol, o.position_side),
            ),
            client_order_id: vike_exec::recon::external_coid(
                &venue,
                &format!("DROP-{}-{}-{ts}", o.symbol, o.position_side),
            ),
            venue: key.0,
            symbol: key.1,
            side,
            last_qty: pos.size.abs(),
            last_px: pos.avg_px,
            commission: 0.0,
            commission_asset: ustr::ustr(""),
            liquidity_side: vike_model::events::LiquiditySide::Unknown,
            ts,
            mark_price: None,
            position_side: o.position_side,
        };
        self.publish_to(idx, vike_model::events::Event::Fill(fill));
        let left = self.eng(idx).account.positions.get(&key).map_or(0.0, |p| p.size);
        if left == 0.0 {
            format!(
                "dropped the local {} {} {} at its own average price {}: zero PnL, no fee",
                o.symbol, o.position_side, o.local_qty, pos.avg_px
            )
        } else {
            format!(
                "NOT dropped: the engine refused the closing fill, {} still reads {left}",
                o.symbol
            )
        }
    }
}

/// The structured line an operator confirm leaves, on the `vike_core::reconcile` target. `refused`
/// is a WARN (the book did not move and the operator should know why); an applied confirm is INFO.
/// (An id the store did not hold has no row to name and logs its own WARN at the call site.)
/// No credential can reach it: an id, a route key, a kind name and a sentence this crate wrote.
pub(crate) fn log_confirm(
    refused: bool,
    id: u64,
    account: &str,
    kind: DivergenceKind,
    outcome: &str,
) {
    let kind = format!("{kind:?}");
    if refused {
        tracing::warn!(
            target: "vike_core::reconcile", id, account, kind = %kind, outcome,
            "reconcile confirm REFUSED"
        );
    } else {
        tracing::info!(
            target: "vike_core::reconcile", id, account, kind = %kind, outcome,
            "reconcile confirm applied"
        );
    }
}

#[cfg(test)]
mod cover_tests {
    use super::*;

    /// The table the module doc states: what each cover can and cannot judge.
    #[test]
    fn a_pass_judges_only_the_kinds_it_carries_evidence_for() {
        let none = PassCover { positions: false, balance: false, journal: false };
        let all = PassCover { positions: true, balance: true, journal: true };
        let position_kinds = [
            DivergenceKind::PositionDrift,
            DivergenceKind::PositionOnlyExternal,
            DivergenceKind::OrphanLocalPosition,
        ];
        for k in position_kinds {
            assert!(!none.covers(k) && all.covers(k), "{k:?}");
        }
        for k in [
            DivergenceKind::MissingTerminal,
            DivergenceKind::OrphanLocalOrder,
            DivergenceKind::UnknownOrder,
        ] {
            assert!(
                none.covers(k) && all.covers(k),
                "an order report is the venue's own answer: {k:?}"
            );
        }
        assert!(
            !none.covers(DivergenceKind::BalanceDrift) && all.covers(DivergenceKind::BalanceDrift)
        );
        assert!(!none.covers(DivergenceKind::JournalDivergence));
        assert!(all.covers(DivergenceKind::JournalDivergence));
        assert!(
            !all.covers(DivergenceKind::MissingFill),
            "a missed fill is never cleared by absence"
        );
    }
}
