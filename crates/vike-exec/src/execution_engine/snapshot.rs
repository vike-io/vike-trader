//! The journal `Snap` round trip: `snapshot_state` captures the OMS, `from_snapshot` rebuilds it.

use crate::risk::RiskGate;

use super::{ExecutionClient, ExecutionEngine};

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
            // Only the case `quote_asset` cannot say (see the field): told "USDT", the guess's own
            // spelling. Every other engine writes nothing here and hashes as it always did.
            account_ccy_set: self.account_ccy_set && self.quote_asset == super::DEFAULT_QUOTE_ASSET,
            // WHOSE book this is. Only a STATEMENT is written: a tier that is not `Paper`, or one the
            // mount told (`set_tier`), so the seed of `new` (and of a rebuild from a tier-less
            // snapshot) says nothing and the canonical JSON of such an engine is what it always was.
            tier: (self.tier_set || self.tier != vike_model::AccountTier::Paper)
                .then_some(self.tier),
        }
    }

    /// Rebuild an engine from a snapshot (restart/replay). `applied_fills` starts empty
    /// (deliveries are not replayed). The tier is the snapshot's when it carries one
    /// ([`crate::EngineSnapshot::tier`]), so a replay core re-snapshots exactly what it was given;
    /// a snapshot without one (written before the field) seeds `Paper` as [`Self::new`] does and
    /// stays UNSTATED, so it is written back without one.
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
        e.account_ccy_set = snap.account_ccy_set;
        e.refresh_ledger_ccy();
        if let Some(tier) = snap.tier {
            e.set_tier(tier);
        }
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
        // A stored rate stamped in the future is not believed (`Account::clamp_future_rates`).
        e.account.clamp_future_rates(snap.now_ms);
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
    /// ([`crate::Account::restore_folded`]: positions, realized PnL, closed PnLs, balances and an
    /// `Authoritative` balance mode, funding, fees), the THREE fill-dedup sets (the account's
    /// bare-fill ledger, the FSM's and the liquidation lane's — a position restored without its
    /// dedup would book a fill the venue re-delivers on connect a second time, and a dedup
    /// restored without its position would lose it), the gate's rate-window timestamps and
    /// `dropped_terminal_on_live`.
    ///
    /// ⚠ It does not look at [`crate::EngineSnapshot::tier`]: whether the snapshot is THIS account's
    /// book is the caller's question, asked before this call (`vike_core`'s `apply_restored_engines`
    /// refuses a snapshot whose stated tier is not this engine's).
    ///
    /// NOT written: the RISK LIMITS (settings and the run profile are their authority, decisions
    /// 0086 and 0111; a journaled ceiling is not a ceiling), `trading_state` (the caller applies it
    /// only when it is stricter), the route key, the tier, the client, `reduce_only_on_close`,
    /// the DECLARED `extra_symbols`, `quote_asset`, `equity_seed` (configuration), the marks (market data) and
    /// the order registry (a journaled order is a CANDIDATE the venue's first reconcile pass
    /// confirms, decision 0120 — never a registry entry at boot).
    pub fn restore_runtime_state(&mut self, snap: &crate::engine_snapshot::EngineSnapshot) {
        self.account.restore_folded(&snap.account);
        self.seed_seen_trade_ids(snap.seen_trade_ids.iter().cloned());
        self.seen_fsm_trade_ids = snap.seen_fsm_trade_ids.iter().cloned().collect();
        self.seen_liq_ids = snap.seen_liq_ids.iter().cloned().collect();
        self.gate.set_throttle_times(snap.gate_order_times.clone());
        self.dropped_terminal_on_live = snap.dropped_terminal_on_live;
        self.restore_admitted_symbols(snap);
        // The rates it brought back are as old as the snapshot says, and a stamp the snapshot's own
        // clock had not reached is not believed (`Account::clamp_future_rates`).
        self.account.clamp_future_rates(snap.now_ms);
    }

    /// Put back the symbols `Command::AdmitSymbol` admitted before the restart (decision 0173): their
    /// `extra_symbols` entry, their `on_demand` grid row and their contract value. Unlike a declared
    /// leg these are RUNTIME state (an operator traded the symbol), so a restart that kept the
    /// positions and dropped the symbol would hold a position the engine does not know it trades. The
    /// node re-admits each one with fresh facts right after the boot (`vike_tradehub::readmit`); this
    /// is what keeps the engine whole until that lands, and when the lookup fails. A declared leg's
    /// row is never overwritten, the cap holds, and nothing here is journaled (it IS the journal).
    fn restore_admitted_symbols(&mut self, snap: &crate::engine_snapshot::EngineSnapshot) {
        // The MOUNTED symbol's margin row, which an admission may have written (a spot mount on a
        // perp's leverage; `admit_symbol`'s mounted arm) and which the loop below never reaches: it
        // has no `on_demand` grid row. `from_snapshot` brings it back with the whole limits; without
        // this a boot restore margined a 1x spot mount at the engine-wide perp rate until the node's
        // re-admission landed, UNDER-stating the margin it needs. A row this mount already holds is
        // the mount's and wins, as it does for the admitted symbols below.
        if let Some(im) = snap.limits.im_by_symbol.get(&self.symbol) {
            self.gate.limits.im_by_symbol.entry(self.symbol.clone()).or_insert(*im);
        }
        // ...and the MOUNTED symbol's conversion rate, when the mount declared it foreign
        // (`Self::declare_mount_quote_ccy`: a ledger that is not in the mount's money). The loop below
        // never reaches the mount, so without this a restored position on it is valued with no rate at
        // all until the node's first admission lands. The rate is as old as the snapshot (stale by the
        // window, so opening stays denied; the node renews it right after the boot), never invented:
        // a snapshot that holds none, or lists the mount under another currency, restores none.
        if let Some(fxs) = snap.account.fx.as_ref() {
            self.account.restore_declared_rate(fxs, &self.symbol);
        }
        for (symbol, row) in &snap.limits.grid_by_symbol {
            if !row.on_demand || *symbol == self.symbol {
                continue;
            }
            if !self.accepts_symbol(symbol) && self.extra_symbols.len() >= crate::MAX_EXTRA_SYMBOLS
            {
                continue;
            }
            let value = snap
                .account
                .inverse
                .iter()
                .find(|(s, _)| s == symbol)
                .map(|(_, v)| vike_model::ContractValue::inverse(*v))
                .or_else(|| {
                    snap.account
                        .mult
                        .iter()
                        .find(|(s, _)| s == symbol)
                        .map(|(_, v)| vike_model::ContractValue::linear(*v))
                });
            // A symbol quoted in a foreign currency comes back through the fx book: the snapshot
            // holds its BASE value and its currency's rate, and the account rebuilds the effective
            // value from them (the node then re-admits it with a fresh rate; until that lands the
            // restored rate is as old as the snapshot, so an opening order is refused and a reduce
            // is not). One the snapshot holds no rate for (a hand-edited snapshot only) is NOT
            // accepted at all: its fills are dropped as an unadmitted symbol's are, because folding
            // them at the symbol's own unit is the par valuation a restore must never produce.
            let foreign = match snap.account.fx.as_ref() {
                None => false,
                Some(fxs) => {
                    let base = value.unwrap_or_else(|| self.account.contract_value_default());
                    match self.account.restore_fx_symbol(fxs, symbol, base) {
                        crate::account::FxRestore::NotForeign => false,
                        crate::account::FxRestore::Restored => true,
                        crate::account::FxRestore::Unusable => continue,
                    }
                }
            };
            if !self.accepts_symbol(symbol) {
                self.extra_symbols.push(symbol.clone());
            }
            if self.gate.limits.grid_by_symbol.get(symbol).is_none_or(|g| g.on_demand) {
                self.gate.limits.grid_by_symbol.insert(symbol.clone(), row.clone());
                // ...and the margin rate the admission stated for it (its `im_by_symbol` row), which
                // is runtime state like the grid row and is guarded like it: a restart that dropped
                // it would margin a 1x spot symbol at the engine-wide perp rate until the node's
                // re-admission landed, and a row the mount already holds is the mount's.
                if let Some(im) = snap.limits.im_by_symbol.get(symbol) {
                    self.gate.limits.im_by_symbol.entry(symbol.clone()).or_insert(*im);
                }
            }
            if !foreign && let Some(value) = value {
                self.account.admit_contract_value(symbol, value);
            }
        }
    }
}
