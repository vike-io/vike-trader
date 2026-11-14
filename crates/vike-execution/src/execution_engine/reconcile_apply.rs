//! The engine's reconcile surface: drift diff, snapshot seed and reap, re-registration, teardown.

use indexmap::IndexMap;
use std::collections::HashSet;
use vike_model::events::{Event, PositionSide};

use crate::bus::{EventBusOutbox, EventHandler};

use super::{
    DRIFT_ABS_TOLERANCE, Engine, ExecutionClient, LOCAL_VIEW_QUANTITY_TOLERANCE, ReconcileSnapshot,
    drift_diverges,
};

#[cfg(doc)]
use crate::Account;

impl<C: ExecutionClient> Engine<C> {
    /// Read-only drift audit: DIFF the currently-folded [`Account`]/registry against the incoming
    /// venue-truth `snapshot`, returning one human-readable warning per divergence (position size,
    /// authoritative balance, or the open-order set) beyond `drift_diverges`'s tolerance; empty
    /// = no drift. It ONLY detects; it writes nothing. Each message names venue, symbol and the
    /// local-vs-venue values. No runtime path calls it: the vike-core `Command::ApplySnapshot` arm
    /// that ran it before [`Self::apply_snapshot`] was deleted with that dead second reconcile path
    /// (unit A2). The live reconcile is `Command::ReconcileReports` (`crate::recon::diff`).
    pub fn diff_snapshot(&self, snapshot: &ReconcileSnapshot) -> Vec<String> {
        let mut warnings = Vec::new();
        let venue = &self.venue;

        // --- positions: each venue-truth (symbol, side) leg vs the locally-folded size ---
        let sides = &snapshot.position_sides;
        let mut seen: HashSet<(String, PositionSide)> = HashSet::new();
        for (i, (symbol, venue_qty)) in snapshot.positions.iter().enumerate() {
            let side = sides.get(i).map_or(PositionSide::Both, |s| s.1);
            seen.insert((symbol.clone(), side));
            let local_quantity = self
                .account
                .positions
                .get(&(ustr::Ustr::from(venue.as_str()), ustr::Ustr::from(symbol.as_str()), side))
                .map(|p| p.size)
                .unwrap_or(0.0);
            if drift_diverges(local_quantity, *venue_qty) {
                warnings.push(format!(
                    "DRIFT position {venue}/{symbol}[{side}]: local {local_quantity} vs venue {venue_qty}"
                ));
            }
        }
        // Local legs the venue snapshot never mentioned (venue implies flat) — a stranded local
        // ghost the seed would silently zero out. Skips legs already diffed above and sub-tolerance
        // dust; only THIS engine's venue.
        for ((v, symbol, side), entry) in &self.account.positions {
            if v.as_str() == venue
                && entry.size.abs() > DRIFT_ABS_TOLERANCE
                && !seen.contains(&(symbol.to_string(), *side))
            {
                warnings.push(format!(
                    "DRIFT position {venue}/{symbol}[{side}]: local {} vs venue 0 (venue reports flat)",
                    entry.size
                ));
            }
        }

        // --- balance: only when the snapshot carries an authoritative (non-zero) balance, mirroring
        // apply_snapshot's own `snapshot.balance != 0.0` overwrite guard (a zero balance is "not
        // reported", not "flat", so it is never diffed) ---
        if snapshot.balance != 0.0 && drift_diverges(self.account.balance, snapshot.balance) {
            warnings.push(format!(
                "DRIFT balance {venue}: local {} vs venue {}",
                self.account.balance, snapshot.balance
            ));
        }

        // --- open orders: the set of live order client ids we track vs the venue's reported open-order set ---
        let venue_order_client_ids: HashSet<&str> =
            snapshot.open_orders.iter().map(|mo| mo.order_client_id()).collect();
        let local_live: HashSet<&str> = self
            .registry
            .iter()
            .filter(|(_, mo)| mo.status.is_live())
            .map(|(order_client_id, _)| order_client_id.as_str())
            .collect();
        let venue_only = venue_order_client_ids.difference(&local_live).count();
        let local_only = local_live.difference(&venue_order_client_ids).count();
        if venue_only > 0 || local_only > 0 {
            warnings.push(format!(
                "DRIFT open-orders {venue}: local {} live vs venue {} open \
                 ({venue_only} venue-only, {local_only} local-only)",
                local_live.len(),
                venue_order_client_ids.len()
            ));
        }

        warnings
    }

    /// Seed Account positions (size + average_price per (symbol, side)) and the open-order registry
    /// from a reconcile snapshot. A net/spot snapshot (no position_sides) writes the
    /// (venue, sym, 'BOTH') key. Non-zero snapshot balance seeds cash so equity_now() =
    /// real_wallet_balance + unrealized instead of PnL-from-zero.
    ///
    /// No runtime path calls it (its `Command::ApplySnapshot` arm was deleted, unit A2): it stays
    /// for the frozen `fixtures/r5/` "snapshot" acts and the engine's own tests. ⚠ Its reap
    /// terminalizes every live local order absent from `snapshot.open_orders`, which is why the
    /// live reconcile never routes through it.
    pub fn apply_snapshot(&mut self, snapshot: &ReconcileSnapshot) {
        let sides = &snapshot.position_sides;
        // (sym, side) -> average_price, parallel to positions
        let mut average_by_key: IndexMap<(String, PositionSide), f64> = IndexMap::new();
        for (i, (symbol, avg)) in snapshot.position_average_price.iter().enumerate() {
            let side = sides.get(i).map_or(PositionSide::Both, |s| s.1);
            average_by_key.insert((symbol.clone(), side), *avg);
        }
        for (i, (symbol, qty)) in snapshot.positions.iter().enumerate() {
            assert!(
                self.accepts_symbol(symbol),
                "snapshot symbol {symbol} not accepted by engine (symbol {}, extra {:?})",
                self.symbol,
                self.extra_symbols
            );
            let side = sides.get(i).map_or(PositionSide::Both, |s| s.1);
            let avg = average_by_key.get(&(symbol.clone(), side)).copied().unwrap_or(0.0);
            // Interned/enum key, built like the fill fold's.
            let key: crate::account::PositionKey =
                (ustr::Ustr::from(self.venue.as_str()), ustr::Ustr::from(symbol.as_str()), side);
            // Margin-mode carrier: a venue-REPORTED mode for this row (`position_margin`,
            // index-aligned like `position_sides`) WINS the overwrite, including a Cross report
            // flipping a stale local Isolated back. When the venue reported nothing (empty/short
            // vec: spot snapshots, deribit), the prior entry's carrier is carried forward (as in
            // `Account::fold`) so the overwrite never silently flips an isolated position to
            // cross. Cross/None are serialization no-ops on the state hash (`skip_serializing_if`).
            let prior = self.account.positions.get(&key).copied().unwrap_or_default();
            let (margin_mode, isolated_margin) = match snapshot.position_margin.get(i) {
                Some((_, m, iso)) => (*m, *iso),
                None => (prior.margin_mode, prior.isolated_margin),
            };
            self.account.positions.insert(
                key,
                crate::PositionEntry {
                    size: *qty,
                    average_price: avg,
                    margin_mode,
                    isolated_margin,
                },
            );
        }
        // A reconcile snapshot's `position_mark_price` IS the venue's own mark for the position,
        // sampled at the reconcile cadence, so it is filed as a genuine venue mark
        // (`MarkSource::ReconcileMark`) and OWNS the account slot for the RECONCILE window
        // (`reconcile_staleness_ms`, default 150s, not the streamed `mark_staleness_ms`) on EVERY
        // venue that reports one, mark stream or not: the price the venue values the position at.
        // The window COVERS the reconcile cadence (default 60s), so ownership is continuous
        // between passes, yet degrades: if reconcile STOPS, closes/ticks reclaim the slot.
        for (symbol, mark) in &snapshot.position_mark_price {
            if *mark > 0.0 {
                self.account.set_mark_from(
                    &self.venue,
                    symbol,
                    *mark,
                    crate::MarkSource::ReconcileMark,
                    self.now_ms,
                );
                self.price_board.set_mark(&self.venue, symbol, *mark, self.now_ms);
            }
        }
        for mo in &snapshot.open_orders {
            self.registry.insert(mo.order_client_id().to_string(), mo.clone());
        }
        // --- Reap stale local orders (continuous-reconcile terminalization) ---
        // The seed above is INSERT-ONLY, so a lost cancel-ack would leave a phantom-live local
        // order forever. Any order WE still hold live whose order client id is absent from the venue's
        // open-order set is terminalized by a synthesized local `OrderCanceled` driven through the
        // SAME FSM path the live fold uses (`on_event` → `OrderRecord::apply` → `on_order_event`
        // capture + `persist_order`), so a mounted strategy learns of it; registry state is never
        // mutated directly. Contract:
        //   * A synthesized cancel carries NO trade_id: `seen_trade_ids`/`seen_fsm_trade_ids` and
        //     the reconnect-resync fill-replay dedup are unaffected.
        //   * Idempotent: a re-applied snapshot finds the order terminal (not live), and a
        //     re-driven invalid transition is dropped by the FSM.
        //   * The cancel transition self-guards the pre-ack race: a still-`Submitted` order is live
        //     but NOT cancelable, so `apply` drops it until its real venue ack lands.
        //   * Only THIS engine's venue is reaped, in `registry` insertion order (byte-identical
        //     `EngineSnapshot` round trips).
        //   * A RARE command path (never the hot fold): a warn per reaped order is fine.
        let venue_open: HashSet<&str> =
            snapshot.open_orders.iter().map(|mo| mo.order_client_id()).collect();
        let reap: Vec<String> = self
            .registry
            .iter()
            .filter(|(order_client_id, mo)| {
                mo.request.venue == self.venue
                    && mo.status.is_live()
                    && !venue_open.contains(order_client_id.as_str())
            })
            .map(|(order_client_id, _)| order_client_id.clone())
            .collect();
        for order_client_id in reap {
            tracing::warn!(
                target: "vike_execution::reconcile",
                venue = %self.venue,
                order_client_id = %order_client_id,
                "reconcile reap: local order absent from venue open-orders — terminalizing (synthetic OrderCanceled)"
            );
            let event = Event::OrderCanceled(vike_model::events::OrderCanceled {
                order_client_id,
                reason: "reconcile-reap".into(),
                timestamp: self.now_ms,
            });
            // Route through the ONE FSM apply site; the throwaway outbox stays empty (a lifecycle
            // fold publishes nothing).
            let mut outbox = EventBusOutbox::default();
            self.on_event(&event, &mut outbox);
        }
        if snapshot.balance != 0.0 {
            self.account.balance = snapshot.balance;
            // A snapshot reseed is a balance sync like `apply_account_state`, so move the
            // cash-reconcile realized-PnL baseline with it; a stale one would make `diff_balance`
            // double-count realized PnL and false-flag a `BalanceDrift`. Read ONLY by
            // `diff_balance` (inert unless `VIKE_RECONCILE_BALANCE` is on) and not snapshotted.
            self.account.realized_pnl_at_balance_sync = Some(self.account.realized_pnl);
        }
    }

    /// Operator-gated order-loss RECOVERY (recon `JournalDivergence` re-registration): INSERT-ONLY
    /// re-seed of venue-reported orders the local registry has lost, each rebuilt from its
    /// `OrderStatusReport` — [`Self::apply_snapshot`]'s open-order seed (an adopted
    /// `created_ms: None` order the stuck-order watchdog never sweeps) WITHOUT the reap. An order client id
    /// already in the registry is left untouched (idempotent, never clobbers fresher local state);
    /// a report with no `order_client_id` is skipped. This restores the order's EXISTENCE, status,
    /// filled qty, venue id and the terms its venue's row STATED (limit, trigger, trail); a term the
    /// row did not state stays unset. Returns the count re-registered. Called from `confirm_reconcile`
    /// (an operator action) and the restore pass's adoption. Reconcile cadence, never the fold.
    pub fn reregister_orders(&mut self, reports: &[vike_model::OrderStatusReport]) -> usize {
        let mut n = 0;
        for r in reports {
            let Some(order_client_id) = r.order_client_id.as_deref() else { continue };
            if self.registry.contains_key(order_client_id) {
                continue; // already known locally — never clobber
            }
            let Some(order) = vike_model::OrderRecord::from_status_report(r) else { continue };
            self.registry.insert(order_client_id.to_string(), order);
            n += 1;
        }
        n
    }

    /// Mark the executions an adopted order's report already holds in its `filled_qty`, so the venue's replay of
    /// one (IBKR replays the day's executions at every rebuilt connection, under an id the engine never saw) is not
    /// counted again. `adopted` are the reports [`Self::reregister_orders`] just seeded; a row of `fills` names its
    /// order by order client id or, carrying none, by the report's venue order id. Only the FSM's set is written: the bare `Fill`
    /// lane keeps its own ledger, so the position folds as before.
    ///
    /// An order whose rows add up to MORE than its report is left unmarked: a pass reads orders before fills, so a
    /// fill landed between the two reads, nothing says which row it is, and a new fill marked here would never count.
    pub fn mark_adopted_fills(
        &mut self,
        adopted: &[vike_model::OrderStatusReport],
        fills: &[vike_model::FillReport],
    ) {
        for report in adopted {
            let Some(order_client_id) = report.order_client_id.as_deref() else { continue };
            let own: Vec<&vike_model::FillReport> = fills
                .iter()
                .filter(|f| match f.order_client_id.as_deref() {
                    Some(named) => named == order_client_id,
                    None => {
                        !f.venue_order_id.is_empty() && f.venue_order_id == report.venue_order_id
                    }
                })
                .collect();
            let reported: f64 = own.iter().map(|f| f.last_qty).sum();
            if reported <= report.filled_qty + LOCAL_VIEW_QUANTITY_TOLERANCE {
                let day = crate::account::fill_window::day_of(self.now_ms);
                self.seen_fsm_trade_ids.extend(own.iter().map(|f| (f.trade_id.to_string(), day)));
            }
        }
    }

    /// Read-only OWNED snapshot of this engine's local state for the reconcile driver
    /// (`recon::diff` consumes it via [`crate::recon::ReconcileLocalState::as_view`]): the order
    /// registry, the fill-dedup trade-id set, and `account.positions` filtered to THIS venue as
    /// `(symbol, position_side) -> signed qty`. A pure read, so the runtime can call it on the
    /// fold thread and hand the result to the reconcile driver's own thread.
    pub fn local_view(&self) -> crate::recon::ReconcileLocalState {
        let positions = self
            .account
            .positions
            .iter()
            .filter(|((v, _symbol, _side), _)| v.as_str() == self.venue)
            // `ReconcileLocalState` keys the symbol as a `String` (compared against venue-report
            // symbols); the interned symbol is rendered out here, once per pass.
            .map(|((_v, symbol, side), entry)| ((symbol.to_string(), *side), entry.size))
            .collect();
        crate::recon::ReconcileLocalState {
            venue: self.venue.clone(),
            orders: self.registry.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            seen_trade_ids: self.seen_trade_ids().map(str::to_string).collect(),
            positions,
            quantity_tolerance: LOCAL_VIEW_QUANTITY_TOLERANCE,
            // Cash slice for the first-class cash reconcile (`recon::diff_balance`). Read-only
            // snapshot of the account's cash state; inert unless `VIKE_RECONCILE_BALANCE` is on.
            cash: crate::recon::ReconcileLocalCash {
                balance: self.account.balance,
                realized_pnl: self.account.realized_pnl,
                realized_at_sync: self.account.realized_pnl_at_balance_sync,
                mode: self.account.balance_mode,
            },
        }
    }

    /// Phase one of a teardown spanning SEVERAL engines: raise this engine's client stop flags and
    /// return at once, joining nothing. Forwards to [`ExecutionClient::begin_detach`], whose doc
    /// carries the cost model and the two rules an override must hold. Touches NO engine state.
    /// Optional: [`Self::shutdown`] alone is a complete teardown for one engine; a caller that
    /// skips this only pays the serial cost.
    pub fn begin_shutdown(&mut self) {
        self.client.begin_detach();
    }

    /// Symmetric detach (the bus unsubscribe half is the core loop's ownership in Rust).
    ///
    /// A multi-engine caller should run [`Self::begin_shutdown`] over every engine FIRST — this
    /// method joins the client's threads, and joining one venue while the next has not yet been
    /// told to stop is what makes a teardown cost one wind-down per venue.
    pub fn shutdown(&mut self) {
        self.client.detach();
    }
}
