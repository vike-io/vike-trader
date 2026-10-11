//! The journal `Snap` round trip: `snapshot_state` captures the OMS, `from_snapshot` rebuilds it.

use crate::risk::RiskGate;

use super::{ExecutionClient, ExecutionEngine};

#[cfg(doc)]
use super::EngineMode;

impl<C: ExecutionClient> ExecutionEngine<C> {
    /// Capture the full OMS state (journal `Snap` record). Dedup sets are SORTED (canonical).
    pub fn snapshot_state(&self) -> crate::engine_snapshot::EngineSnapshot {
        // The fill ledger lives on `Account` and hands out `&str`s (its stored fingerprints are not
        // journal state), so it needs its own canonicalizer: the same sort-into-a-Vec as `sorted`
        // below, keeping the wire shape (`Vec<String>`, ascending) and the `state_hash` fence.
        let sorted_ids = |ids: &mut dyn Iterator<Item = &str>| {
            let mut v: Vec<String> = ids.map(str::to_string).collect();
            v.sort_unstable();
            v
        };
        let sorted = |s: &std::collections::HashSet<String>| {
            let mut v: Vec<String> = s.iter().cloned().collect();
            v.sort_unstable();
            v
        };
        crate::engine_snapshot::EngineSnapshot {
            venue: self.venue.clone(),
            symbol: self.symbol.clone(),
            quote_asset: self.quote_asset.clone(),
            reduce_only_on_close: self.reduce_only_on_close,
            extra_symbols: self.extra_symbols.clone(),
            trading_state: self.trading_state,
            limits: self.gate.limits.clone(),
            gate_order_times: self.gate.throttle_times(),
            registry: self.registry.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            account: self.account.snapshot(),
            seen_trade_ids: sorted_ids(&mut self.seen_trade_ids()),
            seen_fsm_trade_ids: sorted(&self.seen_fsm_trade_ids),
            seen_liq_ids: sorted(&self.seen_liq_ids),
            dropped_terminal_on_live: self.dropped_terminal_on_live,
            equity_seed: self.equity_seed,
            collect_applied_fills: self.collect_applied_fills,
            now_ms: self.now_ms,
            // `None` for the venue's sole/default account, so its bytes (and `state_hash`) are
            // a v16 snapshot's; see the field.
            route_key: (self.route_key != self.venue).then(|| self.route_key.clone()),
        }
    }

    /// Rebuild an engine from a snapshot (restart/replay). `applied_fills` starts empty
    /// (deliveries are not replayed). ⚠ The snapshot does not carry [`Self::mode`], so a restored
    /// engine reports [`EngineMode::Paper`] — [`Self::new`]'s seed — until its caller sets it.
    pub fn from_snapshot(snap: &crate::engine_snapshot::EngineSnapshot, client: C) -> Self {
        let mut gate = RiskGate::new(snap.limits.clone());
        gate.set_throttle_times(snap.gate_order_times.clone());
        let mut e = ExecutionEngine::new(
            crate::Account::restore(&snap.account),
            gate,
            client,
            &snap.venue,
            &snap.symbol,
        );
        e.quote_asset = snap.quote_asset.clone();
        e.reduce_only_on_close = snap.reduce_only_on_close;
        e.extra_symbols = snap.extra_symbols.clone();
        e.trading_state = snap.trading_state;
        e.registry = snap.registry.iter().cloned().collect();
        e.seed_seen_trade_ids(snap.seen_trade_ids.iter().cloned());
        e.seen_fsm_trade_ids = snap.seen_fsm_trade_ids.iter().cloned().collect();
        e.seen_liq_ids = snap.seen_liq_ids.iter().cloned().collect();
        e.dropped_terminal_on_live = snap.dropped_terminal_on_live;
        e.equity_seed = snap.equity_seed;
        e.collect_applied_fills = snap.collect_applied_fills;
        e.now_ms = snap.now_ms;
        // A labelled account's key (`None` leaves `new`'s `route_key == venue`).
        if let Some(route_key) = &snap.route_key {
            e.route_key = route_key.clone();
        }
        // Re-seed the read-side `price_board` from the restored `Account.marks`. `PriceBoard` sits
        // OUTSIDE `EngineSnapshot` (the journal hash-fence), so a bare restore leaves it EMPTY and
        // `resolved_position_price` would return `Missing` for a symbol whose restored mark is
        // present, until the next tick. Pairs the two stores as `apply_snapshot` does, stamped with
        // the restored core clock; the board's dead-price guard drops a non-positive mark. Inert to
        // the determinism fence (the board is not serialized, marks are outside `state_hash`).
        for ((venue, symbol), mark) in &snap.account.marks {
            e.price_board.set_mark(venue, symbol, *mark, e.now_ms);
        }
        e
    }

    /// Put a journaled engine's FOLDED state back onto THIS engine, which its mount already built
    /// (client, gate limits, route key, mode and every config-derived field stay this engine's) —
    /// the boot-restore half of a `Snap` (`vike_core::replay::restore_for_boot`, applied once at
    /// the core's assembly, never on the fold).
    ///
    /// Written, all in one call so they can never disagree: the account's folded money
    /// ([`crate::Account::restore_folded`]: positions, realized PnL, closed PnLs, balances, funding,
    /// fees), the THREE fill-dedup sets (the account's bare-fill ledger, the FSM's and the
    /// liquidation lane's — a position restored without its dedup would book a fill the venue
    /// re-delivers on connect a second time, and a dedup restored without its position would lose
    /// it), the gate's rate-window timestamps and `dropped_terminal_on_live`.
    ///
    /// NOT written: the RISK LIMITS (settings and the run profile are their authority, decisions
    /// 0086 and 0111; a journaled ceiling is not a ceiling), `trading_state` (the caller applies it
    /// only when it is stricter), the route key, the client, `reduce_only_on_close`,
    /// `extra_symbols`, `quote_asset`, `equity_seed` (configuration), the marks (market data) and
    /// the order registry (a journaled order is a CANDIDATE the venue's first reconcile pass
    /// confirms, decision 0120 — never a registry entry at boot).
    pub fn restore_runtime_state(&mut self, snap: &crate::engine_snapshot::EngineSnapshot) {
        self.account.restore_folded(&snap.account);
        self.seed_seen_trade_ids(snap.seen_trade_ids.iter().cloned());
        self.seen_fsm_trade_ids = snap.seen_fsm_trade_ids.iter().cloned().collect();
        self.seen_liq_ids = snap.seen_liq_ids.iter().cloned().collect();
        self.gate.set_throttle_times(snap.gate_order_times.clone());
        self.dropped_terminal_on_live = snap.dropped_terminal_on_live;
    }
}
