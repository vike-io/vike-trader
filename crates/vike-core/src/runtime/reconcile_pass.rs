//! Fold-thread FRESHNESS of the held-alert store: what a completed reconcile pass says about the
//! rows already held, and the log line an operator confirm leaves.
//!
//! A held row used to be inserted or refreshed and NEVER removed except by a confirm, so a
//! divergence a later pass no longer raised stayed listed (`trade recon ls`) and confirmable: a
//! pass that omitted `AAPL` held an `OrphanLocalPosition`, the next pass reported `AAPL` correctly,
//! and the confirm still closed the local position while the venue held it. Now each pass stamps
//! the account's [`ReconcilePassMark`] (a sequence number and what the pass could say about each kind), every
//! row it raises or refreshes carries that sequence, and a row whose kind the pass COVERED but did
//! not raise is stale: [`CoreThread::prune_stale_reconcile`] removes it, and `confirm_reconcile` refuses it.
//!
//! ## What a pass can say ([`ReconcilePassCover::covers`]) - and what it cannot
//! Only a DELIVERED pass reaches the fold (`ReconcileManager` sends nothing on a failed or not-ready
//! fetch), so a failed pass touches nothing here. A delivered pass is still silent about some kinds:
//! - **positions**: the three position kinds are judged only when the pass carried at least one
//!   position row. `vike_execution::recon::diff` gates its orphan sweep the same way, because an empty
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
//!   It is cleared by PRESENCE instead ([`CoreThread::clear_folded_missing_fills`]): a pass that read
//!   the venue's fill before the exec stream delivered it raised a row the book then folded, and the
//!   next pass removes it once the book holds that very fill.
//!   Orders are judged by every delivered pass: an empty order report is the venue's own answer.
//!
//! Cold path only (a reconcile pass, an operator command), never the per-message fold.

use std::collections::HashMap;

use super::*;

/// What one DELIVERED pass could say about the held kinds (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReconcilePassCover {
    /// The pass carried at least one position row.
    pub(crate) positions: bool,
    /// `reconcile_balance` was on and the venue reported a balance.
    pub(crate) balance: bool,
    /// A journal view provider was set, so the three-way check ran.
    pub(crate) journal: bool,
}

impl ReconcilePassCover {
    /// Whether a pass with this cover would have raised `kind` had it still been true. A TOTAL
    /// match, so a new [`ReconcileDivergenceKind`] is a compile error here rather than a kind that silently
    /// inherits an answer.
    pub(crate) fn covers(self, kind: ReconcileDivergenceKind) -> bool {
        match kind {
            ReconcileDivergenceKind::MissingFill => false,
            ReconcileDivergenceKind::MissingTerminal
            | ReconcileDivergenceKind::OrphanLocalOrder
            | ReconcileDivergenceKind::UnknownOrder
            | ReconcileDivergenceKind::OrderTermsDrift => true,
            ReconcileDivergenceKind::PositionDrift
            | ReconcileDivergenceKind::PositionOnlyExternal
            | ReconcileDivergenceKind::OrphanLocalPosition => self.positions,
            ReconcileDivergenceKind::BalanceDrift => self.balance,
            ReconcileDivergenceKind::JournalDivergence => self.journal,
        }
    }
}

/// The latest delivered pass of ONE account: its sequence number and its [`ReconcilePassCover`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReconcilePassMark {
    pub(crate) sequence: u64,
    pub(crate) cover: ReconcilePassCover,
}

/// Whether `held` is contradicted by its account's latest pass: that pass covered its kind and did
/// not raise it (a raised row carries the pass's `seq`). `false` for an account with no pass yet.
pub(crate) fn is_stale(
    marks: &HashMap<String, ReconcilePassMark>,
    held: &ReconcileHeldAlert,
) -> bool {
    marks
        .get(&held.route_key)
        .is_some_and(|m| held.raised_sequence != m.sequence && m.cover.covers(held.kind))
}

impl<C: ExecutionClient> CoreThread<C> {
    /// Open this account's next pass: bump its sequence and record what it can say. Returns the
    /// sequence the pass's raised rows must carry.
    pub(super) fn open_reconcile_pass(
        &mut self,
        account_key: &str,
        cover: ReconcilePassCover,
    ) -> u64 {
        let seq = self.reconcile_pass.get(account_key).map_or(0, |m| m.sequence) + 1;
        self.reconcile_pass
            .insert(account_key.to_string(), ReconcilePassMark { sequence: seq, cover });
        seq
    }

    /// The confirm door's own re-validation of the row it just took out of the store: `true` (and a
    /// ring note and a WARN, nothing folded) when its account's latest pass contradicts it
    /// ([`is_stale`]). The prune after every pass already removes such rows, so this is the door
    /// checking for itself rather than relying on it.
    pub(super) fn refuse_stale_confirm(&mut self, id: u64, held: &ReconcileHeldAlert) -> bool {
        if !is_stale(&self.reconcile_pass, held) {
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
    pub(super) fn prune_stale_reconcile(&mut self, account_key: &str) {
        let gone: Vec<u64> = self
            .reconcile_alerts
            .iter()
            .filter(|(_, h)| h.route_key == account_key && is_stale(&self.reconcile_pass, h))
            .map(|(&id, _)| id)
            .collect();
        for id in gone {
            if let Some(h) = self.reconcile_alerts.shift_remove(&id) {
                self.note(format!(
                    "RECON cleared #{id} {:?}: the latest pass no longer raises it [account {account_key}]",
                    h.kind
                ));
            }
        }
    }

    /// **A held `MissingFill` whose very fill the book has since folded is cleared by the pass**
    /// (`fills` is this pass's venue fill report). The race it ends: a pass reads the venue's fill
    /// before the exec stream delivers it (cTrader, measured 2026-11-09), raises a `MissingFill`, and
    /// the book folds the same trade a moment later; the row stood until a confirm or a restart.
    ///
    /// ⚠ **Presence, never absence.** The row goes only when ALL of these name the same execution:
    /// the book's fill ledger holds the row's trade id with the row's own fill fingerprint (symbol,
    /// side, qty, price: `Account::has_folded_this_fill`, so a colliding binance or okx trade id that
    /// folded another symbol's fill does not match), and this pass's venue report still lists that
    /// trade on the same venue and symbol under the order the row names (its order client id, or for a
    /// order-client-id-less row the venue order id its `EXT-` id is minted from) and, for our own order, the venue
    /// order id the book knows that order by. A fill the ledger REFUSED as a collision is not in it
    /// under its own fingerprint, so its row, the only trace of it, stays; so does a row whose fill
    /// aged out of the report, or re-encoded its price. Its confirm stays idempotent either way.
    ///
    /// Cold: once per delivered pass, over this account's held rows.
    pub(super) fn clear_folded_missing_fills(
        &mut self,
        index: usize,
        account_key: &str,
        fills: &[vike_model::FillReport],
    ) {
        let engine = self.engine(index);
        let folded = |f: &vike_model::events::FillEvent| {
            engine.account.has_folded_this_fill(f)
                && fills.iter().any(|r| {
                    r.trade_id == f.trade_id
                        && r.venue == f.venue.as_str()
                        && r.symbol == f.symbol.as_str()
                        && match &r.order_client_id {
                            Some(order_client_id) => {
                                *order_client_id == f.order_client_id
                                    && engine
                                        .registry
                                        .get(order_client_id)
                                        .and_then(|o| o.venue_order_id.as_deref())
                                        == Some(r.venue_order_id.as_str())
                            }
                            None => {
                                f.order_client_id
                                    == vike_execution::recon::external_order_client_id(
                                        &r.venue,
                                        &r.venue_order_id,
                                    )
                            }
                        }
                })
        };
        let gone: Vec<u64> = self
            .reconcile_alerts
            .iter()
            .filter(|(_, h)| {
                h.route_key == account_key
                    && h.kind == ReconcileDivergenceKind::MissingFill
                    && h.proposed_events
                        .iter()
                        .any(|e| matches!(e, vike_model::events::Event::Fill(f) if folded(f)))
            })
            .map(|(&id, _)| id)
            .collect();
        for id in gone {
            if self.reconcile_alerts.shift_remove(&id).is_some() {
                self.note(format!(
                    "RECON cleared #{id} MissingFill: the book has folded this very fill (same \
                     venue, symbol, order and trade) [account {account_key}]"
                ));
            }
        }
    }

    /// The fold half of an `OrphanLocalPosition` confirm (and of a flat-unpriced `PositionDrift`
    /// one, which carries the same `ReconcileOrphanPosition`): drop the LOCAL position the venue never
    /// reported by booking ONE closing `Fill` through the ordinary event fold, and say in one
    /// clause what happened (the confirm note carries it).
    ///
    /// ⚠ **What is booked, exactly.** A fill on the opposite side for the position's whole size, at
    /// the position's OWN `average_price`, with no commission: `compute_fill` realizes `(px - average_price) * qty
    /// = 0`, so realized PnL, cash and fees do not move, and no price is invented (the reason the
    /// kind folds nothing under any policy). The one residue is a `0.0` entry in `closed_pnls`
    /// (every closing fill appends its realized PnL), which counts as a flat trade in the stats.
    /// The trade id is minted from the pass clock (`EXT-DROP-…`), so a replayed confirm is the same
    /// fill and a repeat is refused by the fill ledger.
    ///
    /// A spot wallet holding LESS than the book (the spot rule, owner 2026-11-11) names the wallet's figure
    /// in `o.venue_qty`: the same fill then closes only the difference, so the book SHRINKS to the wallet at
    /// its own average price, zero PnL. `venue_qty` `0.0` (every other case) is the whole position, as ever.
    ///
    /// ⚠ **It refuses what it cannot prove is still the phantom.** The qty must STILL be the one
    /// the pass saw (`o.local_quantity`): a strategy fill since then means the row describes a book that
    /// no longer exists, and sweeping the new qty away unseen would be the destructive flatten the
    /// rollout rule exists to prevent. The refusal leaves the position, and the next pass re-raises
    /// the divergence against the real number. The result is verified by reading the book again:
    /// an engine that refuses the symbol (`accepts_symbol`) drops the fill silently.
    pub(super) fn drop_orphan_position(
        &mut self,
        index: usize,
        o: &vike_execution::recon::ReconcileOrphanPosition,
    ) -> String {
        let engine = self.engine(index);
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
        if pos.size != o.local_quantity {
            return format!(
                "NOT dropped: the local {} is now {} where the pass saw {}; the next pass re-raises it",
                o.symbol, pos.size, o.local_quantity
            );
        }
        if !(pos.average_price.is_finite() && pos.average_price > 0.0) {
            return format!(
                "NOT dropped: the local average price {} is not a usable close price",
                pos.average_price
            );
        }
        let ts = self.engine.now_ms;
        let side = vike_model::closing_side(pos.size);
        let fill = vike_model::events::FillEvent {
            trade_id: vike_model::events::TradeId::prefixed(
                "EXT-DROP-",
                format_args!("{venue}-{}-{}-{ts}", o.symbol, o.position_side),
            ),
            order_client_id: vike_execution::recon::external_order_client_id(
                &venue,
                &format!("DROP-{}-{}-{ts}", o.symbol, o.position_side),
            ),
            venue: key.0,
            symbol: key.1,
            side,
            last_qty: (pos.size - o.venue_qty).abs(),
            last_price: pos.average_price,
            commission: 0.0,
            commission_asset: ustr::ustr(""),
            liquidity_side: vike_model::events::LiquiditySide::Unknown,
            timestamp: ts,
            mark_price: None,
            position_side: o.position_side,
        };
        self.publish_to(index, vike_model::events::Event::Fill(fill));
        let left = self.engine(index).account.positions.get(&key).map_or(0.0, |p| p.size);
        if o.venue_qty == 0.0 && left == 0.0 {
            format!(
                "dropped the local {} {} {} at its own average price {}: zero PnL, no fee",
                o.symbol, o.position_side, o.local_quantity, pos.average_price
            )
        } else if o.venue_qty != 0.0
            && (left - o.venue_qty).abs() <= super::reconcile_stops::STOP_COVER_QUANTITY_TOLERANCE
        {
            format!(
                "shrank the local {} {} from {} to the wallet's {left} at its own average price {}: zero \
                 PnL, no fee",
                o.symbol, o.position_side, o.local_quantity, pos.average_price
            )
        } else {
            format!(
                "NOT dropped: the engine refused the closing fill, {} still reads {left}",
                o.symbol
            )
        }
    }
}

/// The `tracing` target of the operator confirm's VERDICT lines (applied, refused), a child of
/// `vike_core::reconcile` so a filter naming the parent still matches. It is its own target so
/// `vike-tradehub` can PIN it onto the log file at `info` (`vike_tradehub::audit::CONFIRM_VERDICT_PIN`)
/// without raising the pass-by-pass `reconcile pass folded` lines beside it: the hub's audit line
/// says only that the confirm reached the core's queue, and the applied verdict is `info`, which the
/// `warn` file level the shipped units run would otherwise drop.
pub const CONFIRM_VERDICT_TARGET: &str = "vike_core::reconcile::confirm";

/// The structured line an operator confirm leaves, on [`CONFIRM_VERDICT_TARGET`]. `refused`
/// is a WARN (the book did not move and the operator should know why); an applied confirm is INFO.
/// (An id the store did not hold has no row to name: `confirm_reconcile` logs its own WARN on the same
/// target.) No credential can reach it: an id, a route key, a kind name and a sentence
/// this crate wrote.
pub(crate) fn log_confirm(
    refused: bool,
    id: u64,
    account: &str,
    kind: ReconcileDivergenceKind,
    outcome: &str,
) {
    let kind = format!("{kind:?}");
    if refused {
        tracing::warn!(
            target: CONFIRM_VERDICT_TARGET, id, account, kind = %kind, outcome,
            "reconcile confirm REFUSED"
        );
    } else {
        tracing::info!(
            target: CONFIRM_VERDICT_TARGET, id, account, kind = %kind, outcome,
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
        let none = ReconcilePassCover { positions: false, balance: false, journal: false };
        let all = ReconcilePassCover { positions: true, balance: true, journal: true };
        let position_kinds = [
            ReconcileDivergenceKind::PositionDrift,
            ReconcileDivergenceKind::PositionOnlyExternal,
            ReconcileDivergenceKind::OrphanLocalPosition,
        ];
        for k in position_kinds {
            assert!(!none.covers(k) && all.covers(k), "{k:?}");
        }
        for k in [
            ReconcileDivergenceKind::MissingTerminal,
            ReconcileDivergenceKind::OrphanLocalOrder,
            ReconcileDivergenceKind::UnknownOrder,
            ReconcileDivergenceKind::OrderTermsDrift,
        ] {
            assert!(
                none.covers(k) && all.covers(k),
                "an order report is the venue's own answer: {k:?}"
            );
        }
        assert!(
            !none.covers(ReconcileDivergenceKind::BalanceDrift)
                && all.covers(ReconcileDivergenceKind::BalanceDrift)
        );
        assert!(!none.covers(ReconcileDivergenceKind::JournalDivergence));
        assert!(all.covers(ReconcileDivergenceKind::JournalDivergence));
        assert!(
            !all.covers(ReconcileDivergenceKind::MissingFill),
            "a missed fill is never cleared by absence"
        );
    }
}
