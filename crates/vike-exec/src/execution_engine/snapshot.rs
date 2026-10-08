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
}
