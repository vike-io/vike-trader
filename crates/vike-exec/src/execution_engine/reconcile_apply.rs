//! The engine's reconcile surface: drift diff, snapshot seed and reap, re-registration, teardown.

use indexmap::IndexMap;
use std::collections::HashSet;
use vike_model::events::Event;

use crate::bus::{EventHandler, Outbox};

use super::{
    DRIFT_ABS_TOL, ExecutionClient, ExecutionEngine, LOCAL_VIEW_QTY_TOL, ReconcileSnapshot,
    drift_diverges,
};

#[cfg(doc)]
use crate::Account;

impl<C: ExecutionClient> ExecutionEngine<C> {
    /// Read-only drift audit: DIFF the currently-folded [`Account`]/registry against the incoming
    /// venue-truth `snapshot`, returning one human-readable warning per divergence (position size,
    /// authoritative balance, or the open-order set) beyond `drift_diverges`'s tolerance; empty
    /// = no drift. It ONLY detects — [`Self::apply_snapshot`] still overwrites local state with
    /// venue truth right after. The vike-core runtime calls it immediately BEFORE `apply_snapshot`
    /// on every ReconcileSnapshot and pushes the lines into the GUI-visible recent-events ring + a
    /// `tracing::warn`, so the overwrite is never silent. Each message names venue, symbol and the
    /// local-vs-venue values.
    pub fn diff_snapshot(&self, snapshot: &ReconcileSnapshot) -> Vec<String> {
        let mut warnings = Vec::new();
        let venue = &self.venue;

        // --- positions: each venue-truth (symbol, side) leg vs the locally-folded size ---
        let sides = &snapshot.position_sides;
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for (i, (sym, venue_qty)) in snapshot.positions.iter().enumerate() {
            let side = sides.get(i).map(|s| s.1.as_str()).unwrap_or("BOTH");
            seen.insert((sym.clone(), side.to_string()));
            let local_qty = self
                .account
                .positions
                .get(&(
                    ustr::Ustr::from(venue.as_str()),
                    ustr::Ustr::from(sym.as_str()),
                    vike_model::events::PositionSide::from(side),
                ))
                .map(|p| p.size)
                .unwrap_or(0.0);
            if drift_diverges(local_qty, *venue_qty) {
                warnings.push(format!(
                    "DRIFT position {venue}/{sym}[{side}]: local {local_qty} vs venue {venue_qty}"
                ));
            }
        }
        // Local legs the venue snapshot never mentioned (venue implies flat) — a stranded local
        // ghost the seed would silently zero out. Skips legs already diffed above and sub-tolerance
        // dust; only THIS engine's venue.
        for ((v, sym, side), entry) in &self.account.positions {
            if v.as_str() == venue
                && entry.size.abs() > DRIFT_ABS_TOL
                && !seen.contains(&(sym.to_string(), side.to_string()))
            {
                warnings.push(format!(
                    "DRIFT position {venue}/{sym}[{side}]: local {} vs venue 0 (venue reports flat)",
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

        // --- open orders: the set of live coids we track vs the venue's reported open-order set ---
        let venue_coids: HashSet<&str> =
            snapshot.open_orders.iter().map(|mo| mo.client_order_id()).collect();
        let local_live: HashSet<&str> = self
            .registry
            .iter()
            .filter(|(_, mo)| mo.status.is_live())
            .map(|(coid, _)| coid.as_str())
            .collect();
        let venue_only = venue_coids.difference(&local_live).count();
        let local_only = local_live.difference(&venue_coids).count();
        if venue_only > 0 || local_only > 0 {
            warnings.push(format!(
                "DRIFT open-orders {venue}: local {} live vs venue {} open \
                 ({venue_only} venue-only, {local_only} local-only)",
                local_live.len(),
                venue_coids.len()
            ));
        }

        warnings
    }

    /// Seed Account positions (size + avg_px per (symbol, side)) and the open-order registry
    /// from a reconcile snapshot. A net/spot snapshot (no position_sides) writes the
    /// (venue, sym, 'BOTH') key. Non-zero snapshot balance seeds cash so equity_now() =
    /// real_wallet_balance + unrealized instead of PnL-from-zero.
    pub fn apply_snapshot(&mut self, snapshot: &ReconcileSnapshot) {
        let sides = &snapshot.position_sides;
        // (sym, side) -> avg_px, parallel to positions
        let mut avg_by_key: IndexMap<(String, String), f64> = IndexMap::new();
        for (i, (sym, avg)) in snapshot.position_avg_px.iter().enumerate() {
            let side = sides.get(i).map(|s| s.1.as_str()).unwrap_or("BOTH");
            avg_by_key.insert((sym.clone(), side.to_string()), *avg);
        }
        for (i, (sym, qty)) in snapshot.positions.iter().enumerate() {
            assert!(
                self.accepts_symbol(sym),
                "snapshot symbol {sym} not accepted by engine (symbol {}, extra {:?})",
                self.symbol,
                self.extra_symbols
            );
            let side = sides.get(i).map(|s| s.1.as_str()).unwrap_or("BOTH");
            let avg = avg_by_key.get(&(sym.clone(), side.to_string())).copied().unwrap_or(0.0);
            // Interned/enum key, built like the fill fold's (`PositionSide::from` normalizes the
            // venue's BOTH/LONG/SHORT label).
            let key: crate::account::PositionKey = (
                ustr::Ustr::from(self.venue.as_str()),
                ustr::Ustr::from(sym.as_str()),
                vike_model::events::PositionSide::from(side),
            );
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
                crate::PositionEntry { size: *qty, avg_px: avg, margin_mode, isolated_margin },
            );
        }
        // A reconcile snapshot's `position_mark_px` IS the venue's own mark for the position,
        // sampled at the reconcile cadence, so it is filed as a genuine venue mark
        // (`MarkSource::ReconcileMark`) and OWNS the account slot for the RECONCILE window
        // (`reconcile_staleness_ms`, default 150s, not the streamed `mark_staleness_ms`) on EVERY
        // venue that reports one, mark stream or not: the price the venue values the position at.
        // The window COVERS the reconcile cadence (default 60s), so ownership is continuous
        // between passes, yet degrades: if reconcile STOPS, closes/ticks reclaim the slot.
        for (sym, mark) in &snapshot.position_mark_px {
            if *mark > 0.0 {
                self.account.set_mark_from(
                    &self.venue,
                    sym,
                    *mark,
                    crate::MarkSource::ReconcileMark,
                    self.now_ms,
                );
                self.price_board.set_mark(&self.venue, sym, *mark, self.now_ms);
            }
        }
        for mo in &snapshot.open_orders {
            self.registry.insert(mo.client_order_id().to_string(), mo.clone());
        }
        // --- Reap stale local orders (continuous-reconcile terminalization) ---
        // The seed above is INSERT-ONLY, so a lost cancel-ack would leave a phantom-live local
        // order forever. Any order WE still hold live whose coid is absent from the venue's
        // open-order set is terminalized by a synthesized local `OrderCanceled` driven through the
        // SAME FSM path the live fold uses (`on_event` → `ManagedOrder::apply` → `on_order_event`
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
            snapshot.open_orders.iter().map(|mo| mo.client_order_id()).collect();
        let reap: Vec<String> = self
            .registry
            .iter()
            .filter(|(coid, mo)| {
                mo.request.venue == self.venue
                    && mo.status.is_live()
                    && !venue_open.contains(coid.as_str())
            })
            .map(|(coid, _)| coid.clone())
            .collect();
        for coid in reap {
            tracing::warn!(
                target: "vike_exec::reconcile",
                venue = %self.venue,
                coid = %coid,
                "reconcile reap: local order absent from venue open-orders — terminalizing (synthetic OrderCanceled)"
            );
            let ev = Event::OrderCanceled(vike_model::events::OrderCanceled {
                client_order_id: coid,
                reason: "reconcile-reap".into(),
                ts: self.now_ms,
            });
            // Route through the ONE FSM apply site; the throwaway outbox stays empty (a lifecycle
            // fold publishes nothing).
            let mut outbox = Outbox::default();
            self.on_event(&ev, &mut outbox);
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
    /// `created_ms: None` order the stuck-order watchdog never sweeps) WITHOUT the reap. A coid
    /// already in the registry is left untouched (idempotent, never clobbers fresher local state);
    /// a report with no `client_order_id` is skipped. The report carries no limit/trigger price,
    /// so this restores the order's EXISTENCE, status, filled qty and venue id, not its resting
    /// price. Returns the count re-registered. Called only from `confirm_recon` (an operator
    /// action).
    pub fn reregister_orders(&mut self, reports: &[vike_model::OrderStatusReport]) -> usize {
        let mut n = 0;
        for r in reports {
            let Some(coid) = r.client_order_id.as_deref() else { continue };
            if self.registry.contains_key(coid) {
                continue; // already known locally — never clobber
            }
            let Some(order) = crate::order::ManagedOrder::from_status_report(r) else { continue };
            self.registry.insert(coid.to_string(), order);
            n += 1;
        }
        n
    }

    /// Read-only OWNED snapshot of this engine's local state for the reconcile driver
    /// (`recon::diff` consumes it via [`crate::recon::OwnedLocalState::as_view`]): the order
    /// registry, the fill-dedup trade-id set, and `account.positions` filtered to THIS venue as
    /// `(symbol, position_side) -> signed qty`. A pure read, so the runtime can call it on the
    /// fold thread and hand the result to the reconcile driver's own thread.
    pub fn local_view(&self) -> crate::recon::OwnedLocalState {
        let positions = self
            .account
            .positions
            .iter()
            .filter(|((v, _sym, _side), _)| v.as_str() == self.venue)
            // `OwnedLocalState` stays `String`-keyed (compared against venue-report strings); the
            // interned fold keys are rendered out here, once per pass.
            .map(|((_v, sym, side), entry)| ((sym.to_string(), side.to_string()), entry.size))
            .collect();
        crate::recon::OwnedLocalState {
            venue: self.venue.clone(),
            orders: self.registry.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            seen_trade_ids: self.seen_trade_ids().map(str::to_string).collect(),
            positions,
            qty_tol: LOCAL_VIEW_QTY_TOL,
            // Cash slice for the first-class cash reconcile (`recon::diff_balance`). Read-only
            // snapshot of the account's cash state; inert unless `VIKE_RECONCILE_BALANCE` is on.
            cash: crate::recon::LocalCash {
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
